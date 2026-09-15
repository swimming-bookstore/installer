use anyhow::Context;
use std::path::PathBuf;

use tracing::{span, Span};
use url::Url;

use crate::action::{Action, ActionDescription, ActionState, StatefulAction};
use crate::command::{command, execute_command};

/** Install APT packages, if they are not installed already

Revert deliberately does nothing: these are shared system packages, and removing them
because one install step failed would take other services down with it.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "apt_install")]
pub struct AptInstall {
    packages: Vec<String>,
    missing: Vec<String>,
}

impl AptInstall {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        packages: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> anyhow::Result<StatefulAction<Self>> {
        let packages: Vec<String> = packages
            .into_iter()
            .map(|package| package.as_ref().to_string())
            .collect();

        let mut missing = vec![];
        for package in &packages {
            if !package_installed(package).await {
                missing.push(package.clone());
            }
        }

        let state = if missing.is_empty() {
            tracing::debug!("All APT packages already installed");
            ActionState::Skipped
        } else {
            ActionState::Uncompleted
        };

        Ok(StatefulAction {
            action: Self { packages, missing },
            state,
        })
    }
}

/// Whether `dpkg` considers the package installed
pub async fn package_installed(package: &str) -> bool {
    let output = command("dpkg-query")
        .arg("-W")
        .arg("-f=${Status}")
        .arg(package)
        .output()
        .await;

    match output {
        Ok(output) => {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("install ok installed")
        },
        Err(_) => false,
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "apt_install")]
impl Action for AptInstall {
    fn tracing_synopsis(&self) -> String {
        format!("Install APT packages: {}", self.missing.join(", "))
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "apt_install",
            packages = self.packages.join(","),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            self.tracing_synopsis(),
            vec![String::from(
                "Runs `apt-get update` followed by a non-interactive `apt-get install`",
            )],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        execute_command(command("apt-get").arg("update").arg("-y")).await?;

        execute_command(
            command("apt-get")
                .env("DEBIAN_FRONTEND", "noninteractive")
                .arg("install")
                .arg("-y")
                .args(&self.missing),
        )
        .await?;

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!(
                "Leave the APT packages ({}) installed",
                self.missing.join(", ")
            ),
            vec![String::from(
                "Other services on this host may depend on them",
            )],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        tracing::debug!("Leaving APT packages installed");
        Ok(())
    }
}

/** Add a third party APT repository and the key which signs it */
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "configure_apt_repository")]
pub struct ConfigureAptRepository {
    name: String,
    key_url: Url,
    key_path: PathBuf,
    list_path: PathBuf,
    /// The `deb [...] <url> <suite> <components>` line, minus the signing key option
    repository: String,
}

impl ConfigureAptRepository {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        name: impl AsRef<str>,
        key_url: Url,
        key_path: impl Into<PathBuf>,
        list_path: impl Into<PathBuf>,
        repository: impl AsRef<str>,
    ) -> anyhow::Result<StatefulAction<Self>> {
        let this = Self {
            name: name.as_ref().to_string(),
            key_url,
            key_path: key_path.into(),
            list_path: list_path.into(),
            repository: repository.as_ref().to_string(),
        };

        let state = if this.key_path.exists() && this.list_path.exists() {
            ActionState::Completed
        } else {
            ActionState::Uncompleted
        };

        Ok(StatefulAction {
            action: this,
            state,
        })
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "configure_apt_repository")]
impl Action for ConfigureAptRepository {
    fn tracing_synopsis(&self) -> String {
        format!("Configure the `{}` APT repository", self.name)
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "configure_apt_repository",
            name = self.name,
            list_path = tracing::field::display(self.list_path.display()),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            self.tracing_synopsis(),
            vec![
                format!("Fetch the signing key into `{}`", self.key_path.display()),
                format!("Write `{}`", self.list_path.display()),
            ],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let Self {
            key_url,
            key_path,
            list_path,
            repository,
            ..
        } = self;

        let key = crate::util::fetch_string(key_url).await?;
        crate::util::write_owned_file(key_path, &key, 0o644, None).await?;

        let line = format!(
            "deb [signed-by={key_path}] {repository}\n",
            key_path = key_path.display(),
        );
        crate::util::write_owned_file(list_path, &line, 0o644, None).await?;

        execute_command(command("apt-get").arg("update").arg("-y")).await?;

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Remove the `{}` APT repository", self.name),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        crate::util::remove_file_if_exists(&self.list_path)
            .await
            .with_context(|| format!("Removing `{}`", self.list_path.display()))?;
        crate::util::remove_file_if_exists(&self.key_path)
            .await
            .with_context(|| format!("Removing `{}`", self.key_path.display()))?;
        Ok(())
    }
}
