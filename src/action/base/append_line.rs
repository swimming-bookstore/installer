use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::{span, Span};

use crate::action::{Action, ActionDescription, StatefulAction};
use crate::util::{glob_paths, write_owned_file};

/** Append a line to every file matching a glob, if it is not already present

Paths which do not exist yet (the package that creates them has not been
installed) leave this action uncompleted so execute runs after that package.
Revert restores only the files this action rewrote.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "append_line")]
pub struct AppendLine {
    pattern: String,
    line: String,
    user: Option<String>,
    mode: Option<u32>,
    previous: Vec<(PathBuf, String)>,
}

impl AppendLine {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        pattern: impl AsRef<str>,
        line: impl AsRef<str>,
        user: impl Into<Option<String>>,
        mode: impl Into<Option<u32>>,
    ) -> anyhow::Result<StatefulAction<Self>> {
        let pattern = pattern.as_ref().to_string();
        let line = line.as_ref().to_string();
        let paths = glob_paths(&pattern).await?;

        let mut already = 0;
        let mut missing = 0;
        for path in &paths {
            let contents = tokio::fs::read_to_string(path)
                .await
                .with_context(|| format!("Reading `{}`", path.display()))?;
            if line_present(&contents, &line) {
                already += 1;
            } else {
                missing += 1;
            }
        }

        let action = Self {
            pattern,
            line,
            user: user.into(),
            mode: mode.into(),
            previous: vec![],
        };
        Ok(if missing == 0 && already > 0 {
            StatefulAction::skipped(action)
        } else {
            StatefulAction::uncompleted(action)
        })
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "append_line")]
impl Action for AppendLine {
    fn tracing_synopsis(&self) -> String {
        format!("Append `{}` to `{}`", self.line, self.pattern)
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "append_line",
            pattern = self.pattern.as_str(),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            self.tracing_synopsis(),
            vec![String::from(
                "Skip files that already contain the line; rewrite the rest in place",
            )],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let paths = glob_paths(&self.pattern).await?;
        if paths.is_empty() {
            anyhow::bail!("No files matched `{}`", self.pattern);
        }

        self.previous.clear();
        for path in paths {
            let contents = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("Reading `{}`", path.display()))?;
            if line_present(&contents, &self.line) {
                continue;
            }
            let next = append_line(&contents, &self.line);
            self.previous.push((path.clone(), contents));
            let mode = self.mode.unwrap_or(mode_of(&path).unwrap_or(0o644));
            write_owned_file(&path, &next, mode, self.user.as_deref()).await?;
        }

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Restore `{}`", self.pattern),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        for (path, contents) in self.previous.drain(..) {
            let mode = self.mode.unwrap_or(mode_of(&path).unwrap_or(0o644));
            write_owned_file(&path, &contents, mode, self.user.as_deref()).await?;
        }
        Ok(())
    }
}

fn mode_of(path: &Path) -> Option<u32> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777)
}

pub fn line_present(contents: &str, line: &str) -> bool {
    contents
        .lines()
        .any(|existing| existing.trim() == line.trim())
}

pub fn append_line(contents: &str, line: &str) -> String {
    let mut out = contents.to_string();
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    out
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn detects_and_appends() {
        let line = "hostssl template1 postgres 192.168.1.1/24 scram-sha-256";
        let before = "local   all             postgres                                peer\n";
        assert!(!line_present(before, line));
        let after = append_line(before, line);
        assert!(line_present(&after, line));
        assert_eq!(append_line(&after, line).matches(line).count(), 2);
    }

    #[tokio::test]
    async fn skips_when_every_match_already_has_the_line() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("pg_hba.conf");
        let line = "the-line";
        tokio::fs::write(&path, format!("{line}\n")).await?;

        let action = AppendLine::plan(path.to_str().unwrap(), line, None, None).await?;
        assert_eq!(action.state(), crate::action::ActionState::Skipped);
        Ok(())
    }

    #[tokio::test]
    async fn appends_and_restores() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("pg_hba.conf");
        tokio::fs::write(&path, "keep\n").await?;

        let mut action =
            AppendLine::plan(path.to_str().unwrap(), "added", None, Some(0o644)).await?;
        action.try_execute().await?;
        assert_eq!(tokio::fs::read_to_string(&path).await?, "keep\nadded\n");

        action.try_revert().await?;
        assert_eq!(tokio::fs::read_to_string(&path).await?, "keep\n");
        Ok(())
    }
}
