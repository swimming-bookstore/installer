use anyhow::Context;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nix::unistd::chown;
use tracing::{span, Span};

use crate::action::{Action, ActionDescription, ActionState, StatefulAction};
use crate::util::uid_of;

/** Create a file with the given contents, optionally with an owning user, group, and mode

With `force` the file is overwritten if it exists, which is what generated unit files and
environment files want: they are rendered from settings every run. Without it, a file which
exists with different content is an error rather than a silent clobber.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "create_file")]
pub struct CreateFile {
    pub(crate) path: PathBuf,
    user: Option<String>,
    group: Option<String>,
    mode: Option<u32>,
    buf: String,
    force: bool,
}

impl CreateFile {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        path: impl AsRef<Path>,
        user: impl Into<Option<String>>,
        group: impl Into<Option<String>>,
        mode: impl Into<Option<u32>>,
        buf: String,
        force: bool,
    ) -> anyhow::Result<StatefulAction<Self>> {
        let this = Self {
            path: path.as_ref().to_path_buf(),
            user: user.into(),
            group: group.into(),
            mode: mode.into(),
            buf,
            force,
        };

        let state = if this.path.exists() {
            let metadata = tokio::fs::metadata(&this.path)
                .await
                .with_context(|| format!("Getting metadata for `{}`", this.path.display()))?;

            if !metadata.is_file() {
                anyhow::bail!(
                    "Path `{path}` exists, but is not a file, consider removing it with `rm {path}`",
                    path = this.path.display(),
                );
            }

            if this.force {
                ActionState::Uncompleted
            } else {
                if let Some(user) = &this.user {
                    let expected_uid = uid_of(user)?.as_raw();
                    if metadata.uid() != expected_uid {
                        anyhow::bail!(
                            "`{path}` exists with a different uid ({existing}) than planned ({expected}), consider updating it with `chown -R {expected} {path}`",
                            path = this.path.display(),
                            existing = metadata.uid(),
                            expected = expected_uid,
                        );
                    }
                }

                let existing = tokio::fs::read_to_string(&this.path)
                    .await
                    .with_context(|| format!("Reading `{}`", this.path.display()))?;
                if existing != this.buf {
                    anyhow::bail!(
                        "`{path}` exists with different content than planned, consider removing it with `rm {path}`",
                        path = this.path.display(),
                    );
                }

                tracing::debug!("Creating file `{}` already complete", this.path.display());
                ActionState::Completed
            }
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
#[typetag::serde(name = "create_file")]
impl Action for CreateFile {
    fn tracing_synopsis(&self) -> String {
        format!("Create file `{}`", self.path.display())
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "create_file",
            path = tracing::field::display(self.path.display()),
            user = self.user,
            group = self.group,
            mode = self
                .mode
                .map(|v| tracing::field::display(format!("{v:#o}"))),
            // The buffer can hold a secret, so it is deliberately not a tracing field
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            self.tracing_synopsis(),
            vec![format!(
                "Owned by `{user}:{group}`{mode}",
                user = self.user.as_deref().unwrap_or("root"),
                group = self.group.as_deref().unwrap_or("root"),
                mode = self
                    .mode
                    .map(|mode| format!(", mode `{mode:o}`"))
                    .unwrap_or_default(),
            )],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let Self {
            path,
            user,
            group,
            mode,
            buf,
            force: _,
        } = self;

        crate::util::write_owned_file(path, buf, mode.unwrap_or(0o644), user.as_deref()).await?;

        if let Some(group) = group {
            let gid = crate::util::gid_of(group)?;
            chown(path.as_path(), None, Some(gid))
                .with_context(|| format!("Changing the owner of `{}`", path.display()))?;
        }

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Delete file `{}`", self.path.display()),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        crate::util::remove_file_if_exists(&self.path)
            .await
            .with_context(|| format!("Removing `{}`", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn creates_and_deletes_file() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let path = temp_dir.path().join("unit");
        let mut action =
            CreateFile::plan(&path, None, None, Some(0o600), "contents".into(), false).await?;

        action.try_execute().await?;
        assert_eq!(tokio::fs::read_to_string(&path).await?, "contents");

        action.try_revert().await?;
        assert!(!path.exists());

        Ok(())
    }

    #[tokio::test]
    async fn identical_file_is_already_complete() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let path = temp_dir.path().join("unit");
        tokio::fs::write(&path, "contents").await?;

        let action = CreateFile::plan(&path, None, None, None, "contents".into(), false).await?;
        assert_eq!(action.state(), ActionState::Completed);

        Ok(())
    }

    #[tokio::test]
    async fn differing_file_is_an_error_unless_forced() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let path = temp_dir.path().join("unit");
        tokio::fs::write(&path, "theirs").await?;

        assert!(
            CreateFile::plan(&path, None, None, None, "ours".into(), false)
                .await
                .is_err()
        );

        let mut forced = CreateFile::plan(&path, None, None, None, "ours".into(), true).await?;
        forced.try_execute().await?;
        assert_eq!(tokio::fs::read_to_string(&path).await?, "ours");

        Ok(())
    }
}
