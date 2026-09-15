use anyhow::Context;
use std::fmt;
use std::path::{Path, PathBuf};

use tracing::{span, Span};
use url::Url;

use crate::action::{Action, ActionDescription, StatefulAction};

/// Where a release archive is read from
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum ArchiveSource {
    Url(Url),
    Path(PathBuf),
}

impl fmt::Display for ArchiveSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(url) => write!(f, "{url}"),
            Self::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

/** A release archive and the SHA-256 it has to match before anything is installed from it

A version with no checksum is refused rather than installed unverified.
*/
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ReleaseArchive {
    pub source: ArchiveSource,
    pub sha256: String,
}

impl ReleaseArchive {
    pub fn new(source: ArchiveSource, sha256: impl AsRef<str>) -> anyhow::Result<Self> {
        Ok(Self {
            source,
            sha256: parse_sha256(sha256)?,
        })
    }

    pub fn from_url(url: Url, sha256: impl AsRef<str>) -> anyhow::Result<Self> {
        Self::new(ArchiveSource::Url(url), sha256)
    }

    pub fn from_path(path: impl Into<PathBuf>, sha256: impl AsRef<str>) -> anyhow::Result<Self> {
        Self::new(ArchiveSource::Path(path.into()), sha256)
    }
}

fn parse_sha256(sha256: impl AsRef<str>) -> anyhow::Result<String> {
    let sha256 = sha256.as_ref().trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("`{sha256}` is not a SHA-256: expected 64 hex digits");
    }
    Ok(sha256)
}

/** Read a `.tar.gz` release, verify its checksum, and unpack it into a scratch directory

The archive is downloaded, or read from where it was staged on this host. Nothing is
unpacked unless its SHA-256 matches the one the plan was made with, so a replaced release, a
tampered download or a truncated staged file is refused before it can be installed from.

The scratch directory belongs to the installer, so revert deletes it outright. The actions
which follow pick the binaries and configuration they need out of it.
*/
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
#[serde(tag = "action_name", rename = "fetch_and_unpack_tarball")]
pub struct FetchAndUnpackTarball {
    archive: ReleaseArchive,
    dest: PathBuf,
}

impl FetchAndUnpackTarball {
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn plan(
        archive: ReleaseArchive,
        dest: impl AsRef<Path>,
    ) -> anyhow::Result<StatefulAction<Self>> {
        Ok(StatefulAction::uncompleted(Self {
            archive,
            dest: dest.as_ref().to_path_buf(),
        }))
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "fetch_and_unpack_tarball")]
impl Action for FetchAndUnpackTarball {
    fn tracing_synopsis(&self) -> String {
        format!("Read, verify and unpack `{}`", self.archive.source)
    }

    fn tracing_span(&self) -> Span {
        span!(
            tracing::Level::DEBUG,
            "fetch_and_unpack_tarball",
            source = tracing::field::display(&self.archive.source),
            dest = tracing::field::display(self.dest.display()),
        )
    }

    fn execute_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            self.tracing_synopsis(),
            vec![
                format!("SHA-256 `{}`", self.archive.sha256),
                format!("Unpacked into `{}`", self.dest.display()),
            ],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn execute(&mut self) -> anyhow::Result<()> {
        let Self { archive, dest } = self;

        // A leftover unpack from an interrupted run would confuse the `find` the next
        // actions do, so start from an empty directory
        crate::util::remove_dir_all_if_exists(dest)
            .await
            .with_context(|| format!("Removing `{}`", dest.display()))?;

        let bytes = match &archive.source {
            ArchiveSource::Url(url) => crate::util::fetch_bytes(url).await?,
            ArchiveSource::Path(path) => tokio::fs::read(path)
                .await
                .with_context(|| format!("Reading the staged archive `{}`", path.display()))?,
        };

        let actual = crate::util::sha256_hex(&bytes);
        if actual != archive.sha256 {
            anyhow::bail!(
                "`{source}` does not match its expected SHA-256: expected {expected}, got {actual}. The release may have been replaced, the download tampered with, or the staged file truncated; nothing was installed from it",
                source = archive.source,
                expected = archive.sha256,
            );
        }

        crate::util::unpack_tar_gz(bytes, dest).await?;

        Ok(())
    }

    fn revert_description(&self) -> Vec<ActionDescription> {
        vec![ActionDescription::new(
            format!("Remove the unpacked archive at `{}`", self.dest.display()),
            vec![],
        )]
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn revert(&mut self) -> anyhow::Result<()> {
        crate::util::remove_dir_all_if_exists(&self.dest)
            .await
            .with_context(|| format!("Removing `{}`", self.dest.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn gzip_tar(name: &str, contents: &[u8]) -> anyhow::Result<Vec<u8>> {
        use std::io::Write;

        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, contents)?;
        let tar = builder.into_inner()?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar)?;
        Ok(encoder.finish()?)
    }

    #[test]
    fn rejects_a_non_sha256() {
        let url = url::Url::parse("https://example.com/a.tar.gz").expect("url");
        assert!(ReleaseArchive::from_url(url.clone(), "abc").is_err());
        assert!(ReleaseArchive::from_url(url, "A".repeat(64)).is_ok());
        assert!(ReleaseArchive::from_path("/tmp/a.tar.gz", "abc").is_err());
    }

    #[tokio::test]
    async fn unpacks_a_staged_archive() -> anyhow::Result<()> {
        let bytes = gzip_tar("hello.txt", b"hi")?;
        let sha = crate::util::sha256_hex(&bytes);

        let dir = tempfile::tempdir()?;
        let archive_path = dir.path().join("release.tar.gz");
        tokio::fs::write(&archive_path, &bytes).await?;

        let dest = dir.path().join("unpacked");
        let mut action =
            FetchAndUnpackTarball::plan(ReleaseArchive::from_path(&archive_path, sha)?, &dest)
                .await?;
        action.try_execute().await?;
        assert_eq!(
            tokio::fs::read_to_string(dest.join("hello.txt")).await?,
            "hi"
        );

        action.try_revert().await?;
        assert!(!dest.exists());
        Ok(())
    }

    #[tokio::test]
    async fn refuses_a_checksum_mismatch() -> anyhow::Result<()> {
        let bytes = gzip_tar("hello.txt", b"hi")?;
        let dir = tempfile::tempdir()?;
        let archive_path = dir.path().join("release.tar.gz");
        tokio::fs::write(&archive_path, &bytes).await?;

        let dest = dir.path().join("unpacked");
        let mut action = FetchAndUnpackTarball::plan(
            ReleaseArchive::from_path(&archive_path, "0".repeat(64))?,
            &dest,
        )
        .await?;
        let err = format!(
            "{:#}",
            action.try_execute().await.expect_err("checksum mismatch")
        );
        assert!(err.contains("does not match its expected SHA-256"));
        assert!(!dest.exists());
        Ok(())
    }
}
