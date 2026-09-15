use std::process::ExitCode;

use crate::cli::App;
use crate::planner::Planner;

/// Describe what an install would do, without doing it
pub async fn run<P>(app: &App, planner: P, explain: bool, json: bool) -> anyhow::Result<ExitCode>
where
    P: Planner + 'static,
{
    let plan = crate::cli::plan_with(app, planner).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        println!("{}", plan.describe_install(explain).await?);
    }

    Ok(ExitCode::SUCCESS)
}
