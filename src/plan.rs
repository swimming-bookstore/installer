use std::{path::PathBuf, str::FromStr};

use anyhow::Context;
use owo_colors::OwoColorize;
use semver::Version;
use tokio::sync::broadcast::Receiver;

use crate::{
    action::{fold_errors, Action, ActionDescription, StatefulAction},
    planner::Planner,
};

/**
A sequence of [`Action`]s, plus the [`Planner`] and version which produced it

The plan doubles as the receipt: after an install (or a failed one) it is written to the
path the planner names, with each action's final state, so `uninstall` knows exactly what
was done and in which order to undo it.
*/
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct InstallPlan {
    pub(crate) product: String,
    pub(crate) version: Version,
    pub(crate) actions: Vec<StatefulAction<Box<dyn Action>>>,
    pub(crate) planner: Box<dyn Planner>,
}

impl InstallPlan {
    pub async fn plan<P>(
        planner: P,
        product: impl Into<String>,
        version: &str,
    ) -> anyhow::Result<Self>
    where
        P: Planner + 'static,
    {
        let planner = planner.boxed();
        // Some `Action::plan` calls inspect the machine, and would be misleading (or would
        // fail) if the stage before them has not happened yet
        planner.platform_check().await?;
        planner.pre_install_check().await?;
        let actions = planner.plan().await?;

        Ok(Self {
            planner,
            actions,
            product: product.into(),
            version: parse_version(version)?,
        })
    }

    pub async fn pre_install_check(&self) -> anyhow::Result<()> {
        self.planner.platform_check().await?;
        self.planner.pre_install_check().await
    }

    pub async fn pre_uninstall_check(&self) -> anyhow::Result<()> {
        self.planner.platform_check().await?;
        self.planner.pre_uninstall_check().await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn describe_install(&self, explain: bool) -> anyhow::Result<String> {
        let actions = self.actions.iter().flat_map(|v| v.describe_execute());
        self.describe("install", describe_actions(actions, explain), explain)
            .await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn describe_uninstall(&self, explain: bool) -> anyhow::Result<String> {
        let actions = self.actions.iter().rev().flat_map(|v| v.describe_revert());
        self.describe("uninstall", describe_actions(actions, explain), explain)
            .await
    }

    async fn describe(&self, verb: &str, actions: String, explain: bool) -> anyhow::Result<String> {
        let settings = describe_settings(self.planner.as_ref(), explain).await?;

        Ok(format!(
            "\
            {product} {verb} plan (v{version})\n\
            Planner: {planner_name}{maybe_default_setting_note}\n\
            \n\
            {maybe_plan_settings}\
            Planned actions:\n\
            {actions}\n\
        ",
            product = self.product,
            version = self.version,
            planner_name = self.planner.typetag_name(),
            maybe_default_setting_note = maybe_default_setting_note(&settings),
            maybe_plan_settings = maybe_plan_settings(&settings),
        ))
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn install(
        &mut self,
        cancel_channel: impl Into<Option<Receiver<()>>>,
        current_version: &str,
    ) -> anyhow::Result<()> {
        self.check_compatible(current_version)?;
        self.pre_install_check().await?;

        let mut cancel_channel = cancel_channel.into();

        // Deliberately sequential: the plan *is* the ordering. Steps which may run
        // concurrently are expressed as one composite action which fans out internally.
        for index in 0..self.actions.len() {
            if is_cancelled(&mut cancel_channel) {
                self.write_receipt_best_effort().await;
                anyhow::bail!("Installation cancelled");
            }

            tracing::info!("Step: {}", self.actions[index].tracing_synopsis());
            if let Err(err) = self.actions[index].try_execute().await {
                self.write_receipt_best_effort().await;
                return Err(err);
            }
        }

        self.write_receipt().await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn uninstall(
        &mut self,
        cancel_channel: impl Into<Option<Receiver<()>>>,
        current_version: &str,
    ) -> anyhow::Result<()> {
        self.check_compatible(current_version)?;
        self.pre_uninstall_check().await?;

        let mut cancel_channel = cancel_channel.into();
        let mut errors = vec![];

        for index in (0..self.actions.len()).rev() {
            if is_cancelled(&mut cancel_channel) {
                self.write_receipt_best_effort().await;
                anyhow::bail!("Uninstall cancelled");
            }

            tracing::info!("Revert: {}", self.actions[index].tracing_synopsis());
            if let Err(err) = self.actions[index].try_revert().await {
                errors.push(err);
            }
        }

        self.write_receipt_best_effort().await;
        fold_errors(errors)
    }

    /// Refuse to act on a receipt written by a different version of this installer
    pub fn check_compatible(&self, current_version: &str) -> anyhow::Result<()> {
        let binary_version = parse_version(current_version)?;
        if binary_version != self.version {
            anyhow::bail!(
                "This installer is version `{binary_version}`, but the receipt was written by version `{plan_version}`",
                plan_version = self.version,
            );
        }
        Ok(())
    }

    pub fn receipt_path(&self) -> PathBuf {
        self.planner.receipt_path()
    }

    pub(crate) async fn write_receipt(&self) -> anyhow::Result<()> {
        let receipt_path = self.receipt_path();
        let contents = serde_json::to_string_pretty(self).context("Serializing the receipt")?;

        crate::util::write_atomic(&receipt_path, &format!("{contents}\n"), 0o644, None)
            .await
            .with_context(|| format!("Recording the receipt `{}`", receipt_path.display()))?;

        tracing::debug!("Wrote receipt `{}`", receipt_path.display());
        Ok(())
    }

    /// After a failure the receipt is what makes the failure undoable, so it is written even
    /// though the failure itself is what gets reported
    async fn write_receipt_best_effort(&self) {
        if let Err(err) = self.write_receipt().await {
            tracing::error!("Error saving receipt: {err:#}");
        }
    }
}

fn is_cancelled(cancel_channel: &mut Option<Receiver<()>>) -> bool {
    match cancel_channel {
        Some(channel) => {
            channel.try_recv() != Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        },
        None => false,
    }
}

async fn describe_settings(planner: &dyn Planner, explain: bool) -> anyhow::Result<Vec<String>> {
    let settings = if explain {
        planner.settings()?
    } else {
        planner.configured_settings().await?
    };

    let mut settings = settings
        .into_iter()
        .map(|(k, v)| format!("* {k}: {v}", k = k.bold()))
        .collect::<Vec<_>>();
    settings.sort();
    Ok(settings)
}

fn maybe_default_setting_note(settings: &[String]) -> String {
    if settings.is_empty() {
        String::from(" (with default settings)")
    } else {
        String::new()
    }
}

fn maybe_plan_settings(settings: &[String]) -> String {
    if settings.is_empty() {
        String::new()
    } else {
        format!(
            "Configured settings:\n{settings}\n\n",
            settings = settings.join("\n")
        )
    }
}

fn describe_actions(
    descriptions: impl Iterator<Item = ActionDescription>,
    explain: bool,
) -> String {
    descriptions
        .map(|desc| {
            let ActionDescription {
                description,
                explanation,
            } = desc;

            let mut buf = format!("* {description}");
            if explain {
                for line in explanation {
                    buf.push_str(&format!("\n  {line}"));
                }
            }
            buf
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_version(version: &str) -> anyhow::Result<Version> {
    Version::from_str(version)
        .with_context(|| format!("Parsing the version of this installer (`{version}`)"))
}

#[cfg(test)]
mod tests;
