//! Example of a product installer built on the framework.
//!
//! This binary documents the wiring a real installer needs: clap owns the product CLI,
//! planners live in the product crate, and `installer::cli` runs plan/install/uninstall.

use std::{collections::HashMap, path::PathBuf, process::ExitCode};

use clap::Parser;
use installer::{
    action::base::CreateDirectory,
    action::{Action, StatefulAction},
    cli::{self, arg::Instrumentation, CommandExecute},
    planner::{diff_from_default, Planner},
};

const APP: cli::App = cli::App {
    product: "Example",
    binary_name: "example-installer",
    version: env!("CARGO_PKG_VERSION"),
    env_prefix: "EXAMPLE_INSTALLER",
};

#[derive(Debug, Parser)]
#[clap(version)]
struct Cli {
    #[clap(flatten)]
    instrumentation: Instrumentation,

    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    /// Describe what this install would do
    Plan(Plan),
    /// Carry out the install
    Install(Install),
    /// Undo a previous install, following its receipt
    Uninstall(Uninstall),
}

#[async_trait::async_trait]
impl CommandExecute for Cli {
    async fn execute(self) -> anyhow::Result<ExitCode> {
        match self.subcommand {
            Subcommand::Plan(cmd) => cmd.execute().await,
            Subcommand::Install(cmd) => cmd.execute().await,
            Subcommand::Uninstall(cmd) => cmd.execute().await,
        }
    }
}

#[derive(Debug, Parser)]
struct Plan {
    #[clap(long, global = true)]
    explain: bool,
    #[clap(long, global = true)]
    json: bool,
    #[clap(flatten)]
    planner: Directories,
}

#[async_trait::async_trait]
impl CommandExecute for Plan {
    async fn execute(self) -> anyhow::Result<ExitCode> {
        cli::subcommand::plan::run(&APP, self.planner, self.explain, self.json).await
    }
}

#[derive(Debug, Parser)]
struct Install {
    #[clap(long, action = clap::ArgAction::SetTrue, global = true)]
    no_confirm: bool,
    #[clap(long, action = clap::ArgAction::SetTrue, global = true)]
    explain: bool,
    #[clap(flatten)]
    planner: Directories,
}

#[async_trait::async_trait]
impl CommandExecute for Install {
    async fn execute(self) -> anyhow::Result<ExitCode> {
        cli::subcommand::install::run(&APP, self.planner, self.no_confirm, self.explain, &[]).await
    }
}

#[derive(Debug, Parser)]
struct Uninstall {
    #[clap(long, action = clap::ArgAction::SetTrue, global = true)]
    no_confirm: bool,
    #[clap(long, action = clap::ArgAction::SetTrue, global = true)]
    explain: bool,
    /// Receipt to follow (defaults to the planner's receipt path)
    #[clap(long)]
    receipt: Option<PathBuf>,
    #[clap(flatten)]
    planner: Directories,
}

#[async_trait::async_trait]
impl CommandExecute for Uninstall {
    async fn execute(self) -> anyhow::Result<ExitCode> {
        let receipt = self.receipt.unwrap_or_else(|| self.planner.receipt_path());
        cli::subcommand::uninstall::run(&APP, receipt, self.no_confirm, self.explain).await
    }
}

/// A planner that creates a layout under `--root`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, clap::Parser)]
struct Directories {
    #[clap(long, default_value = "/tmp/example-install", global = true)]
    root: PathBuf,
}

#[async_trait::async_trait]
#[typetag::serde(name = "directories")]
impl Planner for Directories {
    async fn default() -> anyhow::Result<Self> {
        Ok(Self {
            root: PathBuf::from("/tmp/example-install"),
        })
    }

    async fn plan(&self) -> anyhow::Result<Vec<StatefulAction<Box<dyn Action>>>> {
        Ok(vec![
            CreateDirectory::plan(self.root.join("bin"), None, None, Some(0o755), false)
                .await?
                .boxed(),
            CreateDirectory::plan(self.root.join("etc"), None, None, Some(0o755), false)
                .await?
                .boxed(),
        ])
    }

    fn settings(&self) -> anyhow::Result<HashMap<String, serde_json::Value>> {
        let mut map = HashMap::new();
        map.insert(
            "root".into(),
            serde_json::to_value(self.root.display().to_string())?,
        );
        Ok(map)
    }

    async fn configured_settings(&self) -> anyhow::Result<HashMap<String, serde_json::Value>> {
        diff_from_default(self).await
    }

    fn receipt_path(&self) -> PathBuf {
        self.root.join("receipt.json")
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(err) = cli.instrumentation.setup() {
        eprintln!("Error: {err:?}");
        return ExitCode::FAILURE;
    }
    tracing::debug!("{} v{}", APP.binary_name, APP.version);
    cli::run(cli).await
}
