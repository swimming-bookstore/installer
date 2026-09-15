/*! An executable, revertable step of an install

An [`Action`] is the 'atom' of change. Actions are either 'base' actions, which
do one thing to the machine ([`CreateDirectory`](base::CreateDirectory),
[`CreateFile`](base::CreateFile), [`StartSystemdUnit`](base::StartSystemdUnit)), or
'composite' actions, which orchestrate sub-[`Action`]s.

During the plan phase a [`Planner`](crate::planner::Planner) calls an [`Action`]'s `plan`
function, which may accept any arguments and returns a [`StatefulAction`]. `plan` is also
where an action decides it is already done, marking itself
[`Completed`](ActionState::Completed) or [`Skipped`](ActionState::Skipped) so that a second
run is a no-op.

Later [`InstallPlan`](crate::InstallPlan) calls [`try_execute`](StatefulAction::try_execute)
on each [`StatefulAction`], and [`try_revert`](StatefulAction::try_revert) in reverse order
if something fails.

How fine-grained an [`Action`] should be is decided by the unit of reversion: anything that
can fail halfway and needs undoing on its own belongs in its own action.
*/

pub mod base;
mod stateful;

pub use stateful::{ActionState, StatefulAction};

use tracing::Span;

/// An action which can be executed or reverted, wrapped by [`StatefulAction`]
///
/// Prefer [`try_execute`][StatefulAction::try_execute] and
/// [`try_revert`][StatefulAction::try_revert] over calling [`execute`][Action::execute] or
/// [`revert`][Action::revert] directly, so state and tracing are handled.
///
/// Implementors also have an `async fn plan(args...) -> anyhow::Result<StatefulAction<Self>>`.
#[async_trait::async_trait]
#[typetag::serde(tag = "action_name")]
pub trait Action: Send + Sync + std::fmt::Debug + dyn_clone::DynClone {
    /// A synopsis of the action, for tracing
    fn tracing_synopsis(&self) -> String;
    /// A [`tracing::Level::DEBUG`] span named the same as the [`typetag::serde`] entry
    fn tracing_span(&self) -> Span;
    /// What this action would do when executed
    ///
    /// Composite actions should use [`StatefulAction::describe_execute`] on their
    /// sub-actions, not [`execute_description`][Action::execute_description], so completed
    /// sub-actions stay out of the description.
    fn execute_description(&self) -> Vec<ActionDescription>;
    /// What this action would do when reverted
    fn revert_description(&self) -> Vec<ActionDescription>;
    /// Perform the change
    async fn execute(&mut self) -> anyhow::Result<()>;
    /// Undo the change
    async fn revert(&mut self) -> anyhow::Result<()>;
}

dyn_clone::clone_trait_object!(Action);

/// A description of an [`Action`], for humans to review before it runs
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
pub struct ActionDescription {
    pub description: String,
    pub explanation: Vec<String>,
}

impl ActionDescription {
    pub fn new(description: String, explanation: Vec<String>) -> Self {
        Self {
            description,
            explanation,
        }
    }
}

/// The outcome of reverting several steps, each of which was attempted regardless of the
/// others: nothing, the one error, or all of them
pub fn fold_errors(mut errors: Vec<anyhow::Error>) -> anyhow::Result<()> {
    match errors.len() {
        0 => Ok(()),
        1 => Err(errors.remove(0)),
        count => Err(anyhow::anyhow!(
            "{count} steps failed:\n{}",
            errors
                .iter()
                .map(|error| format!("- {error:#}"))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}
