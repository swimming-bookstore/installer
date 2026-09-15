use anyhow::Context;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nix::unistd::{chown, Gid, Group, Uid, User};
use url::Url;
use walkdir::WalkDir;

/// Remove a file, treating "it was not there" as success: a revert should not fail because
/// the thing it undoes is already gone
#[tracing::instrument(skip(path), fields(path = %path.display()))]
pub async fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    tracing::trace!("Removing file");
    match tokio::fs::remove_file(path).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::trace!("Ignoring nonexistent file");
            Ok(())
        },
        e @ Err(_) => e,
    }
}

/// Remove a directory and its contents, treating a missing directory as success
#[tracing::instrument(skip(path), fields(path = %path.display()))]
pub async fn remove_dir_all_if_exists(path: &Path) -> std::io::Result<()> {
    tracing::trace!("Removing directory and all contents");
    match tokio::fs::remove_dir_all(path).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::trace!("Ignoring nonexistent directory");
            Ok(())
        },
        e @ Err(_) => e,
    }
}

/// Whether `path` is a file with something in it
pub async fn file_has_contents(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata.is_file() && metadata.len() > 0,
        Err(_) => false,
    }
}

/// Whether `path` is a directory with at least one entry
pub async fn directory_has_contents(path: &Path) -> bool {
    match tokio::fs::read_dir(path).await {
        Ok(mut entries) => matches!(entries.next_entry().await, Ok(Some(_))),
        Err(_) => false,
    }
}

/// Write a file by way of a temporary sibling which gets its mode and owner first, then is
/// renamed into place
///
/// A file which may hold a secret is therefore never briefly readable by anyone else under
/// its real name, and a failed write never leaves a truncated file behind.
pub async fn write_atomic(
    path: &Path,
    contents: &str,
    mode: u32,
    owner: Option<(Uid, Option<Gid>)>,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no file name",
        )
    })?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    let temp = match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    };

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(mode)
        .open(&temp)
        .await?;
    file.write_all(contents.as_bytes()).await?;
    file.flush().await?;
    drop(file);

    // The mode above is masked by the umask, so set it outright
    tokio::fs::set_permissions(&temp, PermissionsExt::from_mode(mode)).await?;
    if let Some((uid, gid)) = owner {
        chown(temp.as_path(), Some(uid), gid)?;
    }

    tokio::fs::rename(&temp, path).await
}

/// [`write_atomic`] with the owner given by name
pub async fn write_owned_file(
    path: &Path,
    contents: &str,
    mode: u32,
    user: Option<&str>,
) -> anyhow::Result<()> {
    let owner = match user {
        Some(user) => Some((uid_of(user)?, gid_of(user).ok())),
        None => None,
    };
    write_atomic(path, contents, mode, owner)
        .await
        .with_context(|| format!("Writing `{}`", path.display()))
}

pub fn uid_of(user: &str) -> anyhow::Result<Uid> {
    Ok(User::from_name(user)
        .with_context(|| format!("Getting user `{user}`"))?
        .with_context(|| format!("No user `{user}` on this system, create it first"))?
        .uid)
}

pub fn gid_of(group: &str) -> anyhow::Result<Gid> {
    Ok(Group::from_name(group)
        .with_context(|| format!("Getting group `{group}`"))?
        .with_context(|| format!("No group `{group}` on this system, create it first"))?
        .gid)
}

pub fn resolve_owner(
    user: Option<&str>,
    group: Option<&str>,
) -> anyhow::Result<(Option<Uid>, Option<Gid>)> {
    let uid = match user {
        Some(user) => Some(uid_of(user)?),
        None => None,
    };
    let gid = match group {
        Some(group) => Some(gid_of(group)?),
        None => None,
    };
    Ok((uid, gid))
}

/// `chown -R`
pub fn chown_recursive(path: &Path, uid: Option<Uid>, gid: Option<Gid>) -> anyhow::Result<()> {
    if uid.is_none() && gid.is_none() {
        return Ok(());
    }
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry.with_context(|| format!("Walking directory `{}`", path.display()))?;
        chown(entry.path(), uid, gid)
            .with_context(|| format!("Changing the owner of `{}`", entry.path().display()))?;
    }
    Ok(())
}

/// Find the first file named `name` below `root`, deterministically
pub fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .find(|entry| entry.file_name() == name)
        .map(|entry| entry.path().to_path_buf())
}

/// Find the first directory below `root` whose path ends with `suffix`
pub fn find_dir(root: &Path, suffix: &str) -> Option<PathBuf> {
    WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
        .find(|entry| entry.path().ends_with(suffix))
        .map(|entry| entry.path().to_path_buf())
}

/// Download `url` into memory
pub async fn fetch_bytes(url: &Url) -> anyhow::Result<Vec<u8>> {
    tracing::debug!(%url, "Fetching");
    let res = reqwest::Client::new()
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("Fetching `{url}`"))?
        .error_for_status()
        .with_context(|| format!("Fetching `{url}`"))?;
    let bytes = res
        .bytes()
        .await
        .with_context(|| format!("Fetching `{url}`"))?;
    Ok(bytes.to_vec())
}

pub async fn fetch_string(url: &Url) -> anyhow::Result<String> {
    let bytes = fetch_bytes(url).await?;
    String::from_utf8(bytes).context("Output was not valid UTF-8")
}

/// The lower case hex SHA-256 of `bytes`, as `sha256sum` prints it
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;

    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Unpack a `.tar.gz` into `dest`, creating it if needed
pub async fn unpack_tar_gz(bytes: Vec<u8>, dest: &Path) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dest)
        .await
        .with_context(|| format!("Creating directory `{}`", dest.display()))?;

    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
        let mut archive = tar::Archive::new(decoder);
        archive.set_preserve_permissions(true);
        archive
            .unpack(&dest)
            .with_context(|| format!("Unpacking archive into `{}`", dest.display()))
    })
    .await
    .context("Joining a blocking task")?
}

/// `cp -a src/. dst/`
pub async fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let (src, dst) = (src.to_path_buf(), dst.to_path_buf());
    tokio::task::spawn_blocking(move || {
        for entry in WalkDir::new(&src).sort_by_file_name() {
            let entry = entry.with_context(|| format!("Walking directory `{}`", src.display()))?;
            let relative = entry.path().strip_prefix(&src).with_context(|| {
                format!(
                    "`{}` is not below `{}`",
                    entry.path().display(),
                    src.display()
                )
            })?;
            let target = dst.join(relative);

            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&target)
                    .with_context(|| format!("Creating directory `{}`", target.display()))?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Creating directory `{}`", parent.display()))?;
                }
                std::fs::copy(entry.path(), &target).with_context(|| {
                    format!(
                        "Copying `{}` to `{}`",
                        entry.path().display(),
                        target.display()
                    )
                })?;
                let mode = entry
                    .metadata()
                    .with_context(|| format!("Walking directory `{}`", src.display()))?
                    .permissions()
                    .mode();
                std::fs::set_permissions(&target, PermissionsExt::from_mode(mode)).with_context(
                    || format!("Setting mode `{:#o}` on `{}`", mode, target.display()),
                )?;
            }
        }
        Ok(())
    })
    .await
    .context("Joining a blocking task")?
}

/// The distribution codename, read from `/etc/os-release` rather than from `lsb_release`,
/// which may not be installed when the plan is made
pub async fn os_release_codename() -> anyhow::Result<String> {
    let path = Path::new("/etc/os-release");
    let contents = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Reading `{}`", path.display()))?;

    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("VERSION_CODENAME=") {
            return Ok(value.trim_matches('"').to_string());
        }
    }

    anyhow::bail!("`{}` has no VERSION_CODENAME", path.display())
}

pub fn user_exists(user: &str) -> bool {
    matches!(User::from_name(user), Ok(Some(_)))
}

pub fn is_root() -> bool {
    Uid::effective().is_root()
}

/// Planning inspects the machine the way an install would, so it needs the same privileges
pub fn require_root() -> anyhow::Result<()> {
    if !is_root() {
        anyhow::bail!("This step must be run as root: re-run it with `sudo`");
    }
    Ok(())
}

/// The service user has to exist before anything can be owned by it
pub fn require_user(user: &str) -> anyhow::Result<()> {
    if !user_exists(user) {
        anyhow::bail!("No user `{user}` on this system, create it first");
    }
    Ok(())
}

/// The home directory of `user`, as `getent passwd` would report it
pub fn user_home(user: &str) -> anyhow::Result<PathBuf> {
    let user_record = User::from_name(user)
        .with_context(|| format!("Getting user `{user}`"))?
        .with_context(|| format!("No user `{user}` on this system, create it first"))?;
    if !user_record.dir.is_dir() {
        anyhow::bail!(
            "The home directory of `{user}` (`{}`) does not exist",
            user_record.dir.display()
        );
    }
    Ok(user_record.dir)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn hashes_like_sha256sum() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn finds_file_and_dir_by_name() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested)?;
        std::fs::write(nested.join("tool"), "x")?;

        assert_eq!(
            find_file(dir.path(), "tool").as_deref(),
            Some(nested.join("tool").as_path())
        );
        assert_eq!(find_dir(dir.path(), "b").as_deref(), Some(nested.as_path()));
        assert!(find_file(dir.path(), "missing").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn atomic_write_sets_the_mode_and_leaves_no_temporary() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("secret.env");

        write_atomic(&path, "hunter2", 0o600, None).await?;

        assert_eq!(tokio::fs::read_to_string(&path).await?, "hunter2");
        assert_eq!(
            tokio::fs::metadata(&path).await?.permissions().mode() & 0o777,
            0o600
        );
        assert!(!dir.path().join("secret.env.tmp").exists());
        assert!(!dir.path().join("secret.tmp").exists());
        assert!(file_has_contents(&path).await);
        assert!(!file_has_contents(dir.path()).await);
        assert!(directory_has_contents(dir.path()).await);
        Ok(())
    }

    #[tokio::test]
    async fn remove_missing_paths_is_success() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("gone");
        remove_file_if_exists(&missing).await?;
        remove_dir_all_if_exists(&missing).await?;
        Ok(())
    }

    #[tokio::test]
    async fn copy_dir_all_preserves_tree() -> anyhow::Result<()> {
        let src = tempfile::tempdir()?;
        let dst = tempfile::tempdir()?;
        let nested = src.path().join("n");
        tokio::fs::create_dir(&nested).await?;
        tokio::fs::write(nested.join("f"), "hello").await?;

        copy_dir_all(src.path(), dst.path()).await?;
        assert_eq!(
            tokio::fs::read_to_string(dst.path().join("n").join("f")).await?,
            "hello"
        );
        Ok(())
    }

    fn gzip_tar(files: &[(&str, &[u8])]) -> anyhow::Result<Vec<u8>> {
        use std::io::Write;

        let mut builder = tar::Builder::new(Vec::new());
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *contents)?;
        }
        let tar = builder.into_inner()?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar)?;
        Ok(encoder.finish()?)
    }

    #[tokio::test]
    async fn unpacks_a_gzip_tarball() -> anyhow::Result<()> {
        let bytes = gzip_tar(&[("hello.txt", b"hi")])?;
        let dest = tempfile::tempdir()?;
        unpack_tar_gz(bytes, dest.path()).await?;
        assert_eq!(
            tokio::fs::read_to_string(dest.path().join("hello.txt")).await?,
            "hi"
        );
        Ok(())
    }

    #[test]
    fn require_user_names_the_missing_account() {
        let err = require_user("definitely-not-a-real-user-xyz")
            .expect_err("unknown user");
        assert!(err.to_string().contains("definitely-not-a-real-user-xyz"));
    }
}
