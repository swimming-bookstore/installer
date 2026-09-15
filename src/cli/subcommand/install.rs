use std::process::ExitCode;

use owo_colors::OwoColorize;
use tokio::sync::broadcast::Sender;

use crate::cli::{
    ensure_root,
    interaction::{self, PromptChoice},
    signal_channel, App,
};
use crate::planner::Planner;
use crate::InstallPlan;

/// Carry out an install: confirm, run, write a receipt, offer to revert on failure
pub async fn run<P>(
    app: &App,
    planner: P,
    no_confirm: bool,
    explain: bool,
    next_steps: &[String],
) -> anyhow::Result<ExitCode>
where
    P: Planner + 'static,
{
    ensure_root(app)?;

    let stage = planner.typetag_name();
    let mut plan = crate::cli::plan_with(app, planner).await?;

    if !no_confirm {
        let mut currently_explaining = explain;
        loop {
            match interaction::prompt(
                plan.describe_install(currently_explaining).await?,
                PromptChoice::Yes,
                currently_explaining,
            )? {
                PromptChoice::Yes => break,
                PromptChoice::Explain => currently_explaining = true,
                PromptChoice::No => {
                    interaction::clean_exit_with_message("Nothing was changed. Bye!")
                },
            }
        }
    }

    let (tx, rx) = signal_channel()?;

    match plan.install(rx, app.version).await {
        Ok(()) => {
            println!("{}", format!("The `{stage}` stage is done.").bold());
            println!("Receipt: {}", plan.receipt_path().display());
            for note in next_steps {
                println!("{note}");
            }
            Ok(ExitCode::SUCCESS)
        },
        Err(err) => handle_failure(app, &mut plan, err, no_confirm, explain, tx).await,
    }
}

/// Offer to undo what was applied, the way the stage would have been undone later
async fn handle_failure(
    app: &App,
    plan: &mut InstallPlan,
    err: anyhow::Error,
    no_confirm: bool,
    explain: bool,
    tx: Sender<()>,
) -> anyhow::Result<ExitCode> {
    eprintln!("{}", format!("{err:?}").red());

    if no_confirm {
        return Ok(ExitCode::FAILURE);
    }

    eprintln!("{}", "The stage did not finish; it can be reverted.".red());

    let mut currently_explaining = explain;
    loop {
        match interaction::prompt(
            plan.describe_uninstall(currently_explaining).await?,
            PromptChoice::Yes,
            currently_explaining,
        )? {
            PromptChoice::Yes => break,
            PromptChoice::Explain => currently_explaining = true,
            PromptChoice::No => interaction::clean_exit_with_message(
                "Leaving what was applied in place. Its receipt is written, so `uninstall` can still undo it.",
            ),
        }
    }

    plan.uninstall(tx.subscribe(), app.version).await?;
    println!("{}", "What was applied has been reverted.".bold());
    Ok(ExitCode::FAILURE)
}
