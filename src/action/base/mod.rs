/*! Actions which are not specific to any one product */

pub(crate) mod apt;
pub(crate) mod create_directory;
pub(crate) mod create_file;
pub(crate) mod create_symlink;
pub(crate) mod fetch_and_unpack;
pub(crate) mod install_binary;
pub(crate) mod install_tree;
pub(crate) mod require_paths;
pub(crate) mod systemd;

pub use apt::{package_installed, AptInstall, ConfigureAptRepository};
pub use create_directory::CreateDirectory;
pub use create_file::CreateFile;
pub use create_symlink::CreateSymlink;
pub use fetch_and_unpack::{ArchiveSource, FetchAndUnpackTarball, ReleaseArchive};
pub use install_binary::InstallBinary;
pub use install_tree::InstallTree;
pub use require_paths::RequirePaths;
pub use systemd::{
    unit_exists, unit_is_active, unit_path, CreateSystemdUnit, StartSystemdUnit,
};
