use std::path::PathBuf;

use tracing::{span, Span};

use crate::action::base::CreateFile;
use crate::action::{Action, ActionDescription, ActionState, StatefulAction};
use crate::command::{command, command_succeeds, execute_command};

const UNIT_DIR: &str = "/etc/systemd/system";

/** Write a systemd unit and reload the manager

The unit is rendered from settings every run, so it is written unconditionally — the unit
file is a projection of the configuration, not state to be preserved.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "create_systemd_unit")]
pub struct CreateSystemdUnit {
    unit: String,
    path: PathBuf,
    create_file: StatefulAction<CreateFile>,
}

impl CreateSystemdUnit {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(unit: impl AsRef<str>, buf: String) -> anyhow::Result<StatefulAction<Self>> {
        let unit = unit.as_ref().to_string();
        let path = unit_path(&unit);
        let create_file = CreateFile::plan(&path, None, None, Some(0o644), buf, true).await?;

        Ok(StatefulAction::uncompleted(Self {
            unit,
            path,
            create_file,
        }))
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "create_systemd_unit")]
impl Action for CreateSystemdUnit {
    fn tracing_synopsis(&self) -> String {
        format!("Configure the systemd unit `{}`", self.unit)
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "create_systemd_unit",
            unit = self.unit
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            self.tracing_synopsis(),
            vec![
                format!("Write `{}`", self.path.display()),
                String::from("Run `systemctl daemon-reload`"),
            ],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        self.create_file.try_execute().await?;
        daemon_reload().await?;
        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Remove the systemd unit `{}`", self.unit),
            vec![format!("Delete `{}`", self.path.display())],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        self.create_file.try_revert().await?;
        daemon_reload().await?;
        Ok(())
    }
}

async fn daemon_reload() -> anyhow::Result<()> {
    execute_command(command("systemctl").arg("daemon-reload")).await?;
    Ok(())
}

/// Enable (and start) a systemd unit
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "start_systemd_unit")]
pub struct StartSystemdUnit {
    unit: String,
    enable: bool,
    /// Restart a unit which is already running, because its configuration was just rewritten
    restart: bool,
}

impl StartSystemdUnit {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(unit: impl AsRef<str>, enable: bool) -> anyhow::Result<StatefulAction<Self>> {
        let unit = unit.as_ref().to_string();

        // An already running unit was started by an earlier run of this installer, whose
        // unit file this plan rewrites: nothing to do now, but `Completed` rather than
        // `Skipped`, so that undoing this plan still stops the service whose unit it removes
        let state = if unit_is_active(&unit).await {
            tracing::debug!("Systemd unit `{unit}` is already active");
            ActionState::Completed
        } else {
            ActionState::Uncompleted
        };

        Ok(StatefulAction {
            action: Self {
                unit,
                enable,
                restart: false,
            },
            state,
        })
    }

    /// Enable the unit and restart it even if it is already running
    ///
    /// For a unit whose environment file this plan has just rewritten: leaving the old
    /// process running would mean the plan claims a configuration the service is not using.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan_restart(unit: impl AsRef<str>) -> anyhow::Result<StatefulAction<Self>> {
        Ok(StatefulAction::uncompleted(Self {
            unit: unit.as_ref().to_string(),
            enable: true,
            restart: true,
        }))
    }
}

pub async fn unit_is_active(unit: &str) -> bool {
    command_succeeds(command("systemctl").arg("is-active").arg(unit)).await
}

pub fn unit_path(unit: &str) -> PathBuf {
    PathBuf::from(UNIT_DIR).join(unit)
}

pub async fn unit_exists(unit: &str) -> bool {
    unit_path(unit).exists()
}

#[async_trait::async_trait]
#[typetag::serde(name = "start_systemd_unit")]
impl Action for StartSystemdUnit {
    fn tracing_synopsis(&self) -> String {
        match (self.enable, self.restart) {
            (true, true) => format!("Enable and (re)start the systemd unit `{}`", self.unit),
            (true, false) => format!("Enable (and start) the systemd unit `{}`", self.unit),
            (false, _) => format!("Start the systemd unit `{}`", self.unit),
        }
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "start_systemd_unit",
            unit = self.unit,
            enable = self.enable,
            restart = self.restart,
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(self.tracing_synopsis(), vec![])]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let Self {
            unit,
            enable,
            restart,
        } = self;

        if *enable {
            execute_command(command("systemctl").arg("enable").arg(&*unit)).await?;
        }

        let verb = if *restart { "restart" } else { "start" };
        execute_command(command("systemctl").arg(verb).arg(&*unit)).await?;

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Disable (and stop) the systemd unit `{}`", self.unit),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        let mut errors = vec![];

        if self.enable {
            if let Err(e) =
                execute_command(command("systemctl").arg("disable").arg(&self.unit)).await
            {
                errors.push(e);
            }
        }

        // Stop separately from `disable --now`, so a unit the operator already stopped does
        // not fail the revert
        if let Err(e) = execute_command(command("systemctl").arg("stop").arg(&self.unit)).await {
            errors.push(e);
        }

        crate::action::fold_errors(errors)
    }
}
