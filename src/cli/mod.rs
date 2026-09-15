/*! CLI building blocks: `plan`, `install`, `uninstall`, confirmation, sudo, tracing

A consuming binary owns clap: it defines the planner subcommands and calls into
[`subcommand::install::run`], [`subcommand::plan::run`] and
[`subcommand::uninstall::run`].
*/

pub mod arg;
pub mod interaction;
pub mod subcommand;

use std::{ffi::CString, io::IsTerminal, process::ExitCode};

use anyhow::Context;
use owo_colors::OwoColorize;
use tokio::sync::broadcast::{Receiver, Sender};

use crate::planner::Planner;
use crate::util::is_root;
use crate::InstallPlan;

#[async_trait::async_trait]
pub trait CommandExecute {
    async fn execute(self) -> anyhow::Result<ExitCode>;
}

/// Identity of the consuming installer, used for receipts, sudo, and prompts
#[derive(Debug, Clone)]
pub struct App {
    /// Human-readable product name shown in plan descriptions (`Example`)
    pub product: &'static str,
    /// Binary name shown when escalating (`example-installer`)
    pub binary_name: &'static str,
    /// Semver of the consuming binary, stored on receipts
    pub version: &'static str,
    /// Environment variable prefix (`EXAMPLE_INSTALLER`)
    pub env_prefix: &'static str,
}

/// Run the consuming binary, printing errors to stderr
pub async fn run<C>(cli: C) -> ExitCode
where
    C: CommandExecute,
{
    match cli.execute().await {
        Ok(code) => code,
        Err(err) => {
            let report = format!("Error: {err:?}");
            if std::io::stderr().is_terminal() {
                eprintln!("{}", report.red());
            } else {
                eprintln!("{report}");
            }
            ExitCode::FAILURE
        },
    }
}

/// A channel which fires when the operator interrupts us, so an install can stop between
/// actions rather than in the middle of one
pub fn signal_channel() -> anyhow::Result<(Sender<()>, Receiver<()>)> {
    let (sender, receiver) = tokio::sync::broadcast::channel(100);

    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .context("Installing the SIGINT handler")?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("Installing the SIGTERM handler")?;

    let sender_cloned = sender.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(()) = interrupt.recv() => {
                    tracing::warn!("Got SIGINT, stopping after this step");
                    sender_cloned.send(()).ok();
                },
                Some(()) = terminate.recv() => {
                    tracing::warn!("Got SIGTERM, stopping after this step");
                    sender_cloned.send(()).ok();
                },
            }
        }
    });

    Ok((sender, receiver))
}

/// Re-run ourselves under `sudo` if we are not already root
///
/// Variables whose names start with `env_prefix`, plus the usual proxy and Rust diagnostics,
/// are carried across by name (`--preserve-env`), never as `KEY=VALUE` arguments `sudo` would
/// log.
pub fn ensure_root(app: &App) -> anyhow::Result<()> {
    if is_root() {
        return Ok(());
    }

    eprintln!(
        "{}",
        format!(
            "`{}` needs to run as `root`, escalating with `sudo`...",
            app.binary_name
        )
        .yellow()
        .dimmed()
    );

    let mut arguments = vec![
        CString::new("sudo").context("Building the `sudo` argument")?,
        CString::new("--set-home").context("Building the `--set-home` argument")?,
    ];

    let prefix = app.env_prefix;
    let preserved: Vec<String> = std::env::vars_os()
        .filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| {
            matches!(key.as_str(), "RUST_LOG" | "RUST_BACKTRACE" | "SHELL")
                || key.starts_with(prefix)
                || key.starts_with("http_proxy")
                || key.starts_with("https_proxy")
                || key.starts_with("HTTP_PROXY")
                || key.starts_with("HTTPS_PROXY")
        })
        .collect();

    if !preserved.is_empty() {
        arguments.push(
            CString::new(format!("--preserve-env={}", preserved.join(",")))
                .context("Building the `--preserve-env` argument")?,
        );
    }

    for argument in std::env::args() {
        arguments.push(CString::new(argument).context("Building an argument")?);
    }

    let sudo = CString::new("sudo").context("Building the `sudo` program name")?;
    tracing::trace!("Executing `{sudo:?}` with `{arguments:?}`");
    nix::unistd::execvp(&sudo, &arguments).context("Re-running this installer under `sudo`")?;

    Ok(())
}

/// Build a plan from a planner, using this app's product name and version
pub async fn plan_with<P>(app: &App, planner: P) -> anyhow::Result<InstallPlan>
where
    P: Planner + 'static,
{
    InstallPlan::plan(planner, app.product, app.version).await
}
