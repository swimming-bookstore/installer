use anyhow::Context;
use std::path::{Path, PathBuf};

use tracing::{span, Span};

use crate::action::{Action, ActionDescription, ActionState, StatefulAction};
use crate::command::{command, execute_command};

/** Point a symlink at a target, creating or re-pointing it

A real file or directory already sitting at `link` is left alone (and the action skipped) —
the operator put something there deliberately, and replacing it is not ours to do.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "create_symlink")]
pub struct CreateSymlink {
    link: PathBuf,
    target: PathBuf,
    user: Option<String>,
}

impl CreateSymlink {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        link: impl AsRef<Path>,
        target: impl AsRef<Path>,
        user: impl Into<Option<String>>,
    ) -> anyhow::Result<StatefulAction<Self>> {
        let link = link.as_ref().to_path_buf();
        let target = target.as_ref().to_path_buf();

        let state = if link.is_symlink() {
            let existing = tokio::fs::read_link(&link)
                .await
                .with_context(|| format!("Reading `{}`", link.display()))?;
            if existing == target {
                ActionState::Completed
            } else {
                ActionState::Uncompleted
            }
        } else if link.exists() {
            tracing::warn!(
                "`{}` exists and is not a symlink, leaving it unchanged",
                link.display()
            );
            ActionState::Skipped
        } else {
            ActionState::Uncompleted
        };

        Ok(StatefulAction {
            action: Self {
                link,
                target,
                user: user.into(),
            },
            state,
        })
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "create_symlink")]
impl Action for CreateSymlink {
    fn tracing_synopsis(&self) -> String {
        format!(
            "Symlink `{link}` to `{target}`",
            link = self.link.display(),
            target = self.target.display()
        )
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "create_symlink",
            link = tracing::field::display(self.link.display()),
            target = tracing::field::display(self.target.display()),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(self.tracing_synopsis(), vec![])]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let Self { link, target, user } = self;

        if link.is_symlink() {
            crate::util::remove_file_if_exists(link)
                .await
                .with_context(|| format!("Removing `{}`", link.display()))?;
        }

        tokio::fs::symlink(&target, &link).await.with_context(|| {
            format!("Symlinking `{}` to `{}`", target.display(), link.display())
        })?;

        if let Some(user) = user {
            // `chown -h`: own the link itself, never the target it points into
            execute_command(
                command("chown")
                    .arg("-h")
                    .arg(format!("{user}:{user}"))
                    .arg(link),
            )
            .await?;
        }

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Remove the symlink `{}`", self.link.display()),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        if self.link.is_symlink() {
            crate::util::remove_file_if_exists(&self.link)
                .await
                .with_context(|| format!("Removing `{}`", self.link.display()))?;
        }
        Ok(())
    }
}
