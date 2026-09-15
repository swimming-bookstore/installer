use anyhow::Context;
use std::path::{Path, PathBuf};

use tracing::{span, Span};

use crate::action::{Action, ActionDescription, StatefulAction};
use crate::command::{command, execute_command};

/** Install a binary found inside an unpacked release archive

The layout inside a release tarball varies between projects and versions, so the binary is
located by name at execute time rather than assumed from a path.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "install_binary")]
pub struct InstallBinary {
    search_root: PathBuf,
    binary_name: String,
    dest: PathBuf,
    user: Option<String>,
}

impl InstallBinary {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        search_root: impl AsRef<Path>,
        binary_name: impl AsRef<str>,
        dest: impl AsRef<Path>,
        user: impl Into<Option<String>>,
    ) -> anyhow::Result<StatefulAction<Self>> {
        Ok(StatefulAction::uncompleted(Self {
            search_root: search_root.as_ref().to_path_buf(),
            binary_name: binary_name.as_ref().to_string(),
            dest: dest.as_ref().to_path_buf(),
            user: user.into(),
        }))
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "install_binary")]
impl Action for InstallBinary {
    fn tracing_synopsis(&self) -> String {
        format!(
            "Install `{binary}` to `{dest}`",
            binary = self.binary_name,
            dest = self.dest.display()
        )
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "install_binary",
            binary_name = self.binary_name,
            dest = tracing::field::display(self.dest.display()),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            self.tracing_synopsis(),
            vec![format!(
                "Mode `755`, owned by `{}`",
                self.user.as_deref().unwrap_or("root")
            )],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let Self {
            search_root,
            binary_name,
            dest,
            user,
        } = self;

        let source = crate::util::find_file(search_root, binary_name)
            .with_context(|| format!("The release archive did not contain `{binary_name}`"))?;

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Creating directory `{}`", parent.display()))?;
        }

        // `install` rather than `copy`: the destination may be the running binary, and
        // replacing the inode leaves a running process undisturbed
        let owner = user.clone().unwrap_or_else(|| "root".to_string());
        execute_command(
            command("install")
                .arg("-m")
                .arg("0755")
                .arg("-o")
                .arg(&owner)
                .arg("-g")
                .arg(&owner)
                .arg(&source)
                .arg(&*dest),
        )
        .await?;

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Remove `{}`", self.dest.display()),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        crate::util::remove_file_if_exists(&self.dest)
            .await
            .with_context(|| format!("Removing `{}`", self.dest.display()))?;
        Ok(())
    }
}
