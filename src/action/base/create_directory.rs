use anyhow::Context;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::unistd::chown;
use tracing::{span, Span};

use crate::action::{Action, ActionDescription, ActionState, StatefulAction};
use crate::util::{resolve_owner, uid_of};

/** Create a directory, optionally with an owning user, group, and mode

If `force_prune_on_revert` is set the directory is removed on revert even when it has
contents. Otherwise a populated directory is left alone, so reverting a half-finished
install never eats data the operator cares about.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "create_directory")]
pub struct CreateDirectory {
    pub(crate) path: PathBuf,
    user: Option<String>,
    group: Option<String>,
    mode: Option<u32>,
    force_prune_on_revert: bool,
}

impl CreateDirectory {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        path: impl AsRef<Path>,
        user: impl Into<Option<String>>,
        group: impl Into<Option<String>>,
        mode: impl Into<Option<u32>>,
        force_prune_on_revert: bool,
    ) -> anyhow::Result<StatefulAction<Self>> {
        let path = path.as_ref().to_path_buf();
        let user = user.into();
        let group = group.into();
        let mode = mode.into();

        let state = if path.exists() {
            let metadata = tokio::fs::metadata(&path)
                .await
                .with_context(|| format!("Getting metadata for `{}`", path.display()))?;

            if !metadata.is_dir() {
                anyhow::bail!(
                    "Path `{path}` exists, but is not a directory, consider removing it with `rm {path}`",
                    path = path.display(),
                );
            }

            if let Some(user) = &user {
                let expected_uid = uid_of(user)?.as_raw();
                if metadata.uid() != expected_uid {
                    anyhow::bail!(
                        "`{path}` exists with a different uid ({existing}) than planned ({expected}), consider updating it with `chown -R {expected} {path}`",
                        path = path.display(),
                        existing = metadata.uid(),
                        expected = expected_uid,
                    );
                }
            }

            tracing::debug!("Creating directory `{}` already complete", path.display());
            // The directory predates us, so reverting must not delete it
            ActionState::Skipped
        } else {
            ActionState::Uncompleted
        };

        Ok(StatefulAction {
            action: Self {
                path,
                user,
                group,
                mode,
                force_prune_on_revert,
            },
            state,
        })
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "create_directory")]
impl Action for CreateDirectory {
    fn tracing_synopsis(&self) -> String {
        format!("Create directory `{}`", self.path.display())
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "create_directory",
            path = tracing::field::display(self.path.display()),
            user = self.user,
            group = self.group,
            mode = self
                .mode
                .map(|v| tracing::field::display(format!("{v:#o}"))),
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
            force_prune_on_revert: _,
        } = self;

        let (uid, gid) = resolve_owner(user.as_deref(), group.as_deref())?;

        tokio::fs::create_dir_all(&path)
            .await
            .with_context(|| format!("Creating directory `{}`", path.display()))?;
        chown(path.as_path(), uid, gid)
            .with_context(|| format!("Changing the owner of `{}`", path.display()))?;

        if let Some(mode) = mode {
            tokio::fs::set_permissions(&path, PermissionsExt::from_mode(*mode))
                .await
                .with_context(|| format!("Setting mode `{:#o}` on `{}`", *mode, path.display()))?;
        }

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!(
                "Remove the directory `{path}`{qualifier}",
                path = self.path.display(),
                qualifier = if self.force_prune_on_revert {
                    " and its contents"
                } else {
                    " if it is empty"
                },
            ),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        let Self {
            path,
            force_prune_on_revert,
            ..
        } = self;

        if !path.exists() {
            return Ok(());
        }

        let is_empty = path
            .read_dir()
            .with_context(|| format!("Reading directory `{}`", path.display()))?
            .next()
            .is_none();

        if is_empty || *force_prune_on_revert {
            crate::util::remove_dir_all_if_exists(path)
                .await
                .with_context(|| format!("Removing `{}`", path.display()))?;
        } else {
            tracing::debug!("Not removing `{}`, it is not empty", path.display());
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn creates_and_removes_empty_directory() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let test_dir = temp_dir.path().join("create-and-remove");
        let mut action = CreateDirectory::plan(&test_dir, None, None, None, false).await?;

        action.try_execute().await?;
        assert!(test_dir.is_dir());

        action.try_revert().await?;
        assert!(!test_dir.exists(), "Directory should have been removed");

        Ok(())
    }

    #[tokio::test]
    async fn leaves_populated_directory_when_not_pruning() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let test_dir = temp_dir.path().join("populated");
        let mut action = CreateDirectory::plan(&test_dir, None, None, None, false).await?;

        action.try_execute().await?;
        tokio::fs::write(test_dir.join("chain-data"), "precious").await?;
        action.try_revert().await?;

        assert!(test_dir.exists(), "Populated directory should be kept");

        Ok(())
    }

    #[tokio::test]
    async fn existing_directory_is_skipped_both_ways() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let test_dir = temp_dir.path().join("pre-existing");
        tokio::fs::create_dir(&test_dir).await?;

        let mut action = CreateDirectory::plan(&test_dir, None, None, None, true).await?;
        assert_eq!(action.state(), ActionState::Skipped);

        action.try_execute().await?;
        action.try_revert().await?;
        assert!(test_dir.exists(), "A directory we did not create is kept");

        Ok(())
    }
}
