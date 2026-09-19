use tracing::{span, Span};

use crate::action::{Action, ActionDescription, StatefulAction};
use crate::command::{command, command_as, execute_command, execute_command_with_stdin};
use crate::Secret;

/** Run a command, optionally as another user and with secret stdin

The command line is logged; stdin is not. Revert is a no-op: a command that
creates machine state (a role, a password, a database) is undone by later
steps that drop that state, not by guessing an inverse command.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "run_command")]
pub struct RunCommand {
    user: Option<String>,
    program: String,
    args: Vec<String>,
    stdin: Option<Secret>,
    description: String,
}

impl RunCommand {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        program: impl AsRef<str>,
        args: impl IntoIterator<Item = impl AsRef<str>>,
        user: impl Into<Option<String>>,
        stdin: impl Into<Option<Secret>>,
        description: impl AsRef<str>,
    ) -> anyhow::Result<StatefulAction<Self>> {
        Ok(StatefulAction::uncompleted(Self {
            user: user.into(),
            program: program.as_ref().to_string(),
            args: args
                .into_iter()
                .map(|arg| arg.as_ref().to_string())
                .collect(),
            stdin: stdin.into(),
            description: description.as_ref().to_string(),
        }))
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "run_command")]
impl Action for RunCommand {
    fn tracing_synopsis(&self) -> String {
        self.description.clone()
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "run_command",
            program = self.program.as_str(),
            user = self.user.as_deref().unwrap_or("root"),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(self.tracing_synopsis(), vec![])]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let mut cmd = match &self.user {
            Some(user) => command_as(user, &self.program),
            None => command(&self.program),
        };
        cmd.args(&self.args);

        match &self.stdin {
            Some(stdin) => {
                execute_command_with_stdin(&mut cmd, stdin.expose().as_bytes(), &self.description)
                    .await?;
            },
            None => {
                execute_command(&mut cmd).await?;
            },
        }
        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Leave `{}` as-is", self.description),
            vec![String::from(
                "There is no inverse command; later steps drop the state this created",
            )],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        tracing::debug!("No inverse for `{}`", self.description);
        Ok(())
    }
}
