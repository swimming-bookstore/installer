use std::{path::PathBuf, process::ExitCode};

use anyhow::Context;
use owo_colors::OwoColorize;

use crate::cli::{
    ensure_root,
    interaction::{self, PromptChoice},
    signal_channel, App,
};
use crate::InstallPlan;

/// Undo an install, following the receipt it wrote
pub async fn run(
    app: &App,
    receipt_path: PathBuf,
    no_confirm: bool,
    explain: bool,
) -> anyhow::Result<ExitCode> {
    ensure_root(app)?;

    let contents = tokio::fs::read_to_string(&receipt_path)
        .await
        .with_context(|| {
            format!(
                "Reading the receipt `{path}`. Nothing was installed from here, or it was installed with a different data root.",
                path = receipt_path.display()
            )
        })?;
    let mut plan: InstallPlan = serde_json::from_str(&contents).with_context(|| {
        format!(
            "Parsing the receipt `{path}`; it may have been written by an incompatible version of this installer",
            path = receipt_path.display()
        )
    })?;

    if !no_confirm {
        let mut currently_explaining = explain;
        loop {
            match interaction::prompt(
                plan.describe_uninstall(currently_explaining).await?,
                PromptChoice::No,
                currently_explaining,
            )? {
                PromptChoice::Yes => break,
                PromptChoice::Explain => currently_explaining = true,
                PromptChoice::No => {
                    interaction::clean_exit_with_message("Nothing was undone. Bye!")
                },
            }
        }
    }

    let (_tx, rx) = signal_channel()?;
    plan.uninstall(rx, app.version).await?;

    println!("{}", "Undone.".bold());
    Ok(ExitCode::SUCCESS)
}
