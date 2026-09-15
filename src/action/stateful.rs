use anyhow::Context;

use tracing::Instrument;

use super::{Action, ActionDescription};

/// An [`Action`](crate::action::Action) plus the [`ActionState`] which decides whether it
/// still needs to run, or still needs undoing
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone)]
pub struct StatefulAction<A> {
    pub(crate) action: A,
    pub(crate) state: ActionState,
}

/// Where an [`Action`](crate::action::Action) stands relative to the machine
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    /// Done: skipped on install, reverted on uninstall
    Completed,
    /// Half done: run on both install and uninstall, so a partial composite action finishes
    /// or unwinds
    Progress,
    /// Not done: run on install, skipped on uninstall
    Uncompleted,
    /// Deliberately not our business — skipped both ways
    ///
    /// Used by actions which, while planning, find the machine already in the desired state
    /// by means we did not put there and must not undo.
    Skipped,
}

impl StatefulAction<Box<dyn Action>> {
    pub fn tracing_synopsis(&self) -> String {
        self.action.tracing_synopsis()
    }

    pub fn describe_execute(&self) -> Vec<ActionDescription> {
        describe_execute(self.action.as_ref(), self.state)
    }

    pub fn describe_revert(&self) -> Vec<ActionDescription> {
        describe_revert(self.action.as_ref(), self.state)
    }

    pub async fn try_execute(&mut self) -> anyhow::Result<()> {
        try_execute(self.action.as_mut(), &mut self.state).await
    }

    pub async fn try_revert(&mut self) -> anyhow::Result<()> {
        try_revert(self.action.as_mut(), &mut self.state).await
    }
}

impl<A> StatefulAction<A>
where
    A: Action,
{
    pub fn tracing_synopsis(&self) -> String {
        self.action.tracing_synopsis()
    }

    pub fn describe_execute(&self) -> Vec<ActionDescription> {
        describe_execute(&self.action, self.state)
    }

    pub fn describe_revert(&self) -> Vec<ActionDescription> {
        describe_revert(&self.action, self.state)
    }

    pub async fn try_execute(&mut self) -> anyhow::Result<()> {
        try_execute(&mut self.action, &mut self.state).await
    }

    pub async fn try_revert(&mut self) -> anyhow::Result<()> {
        try_revert(&mut self.action, &mut self.state).await
    }

    pub fn state(&self) -> ActionState {
        self.state
    }

    pub fn inner(&self) -> &A {
        &self.action
    }

    /// Type erase the action, so a plan can hold a sequence of differing actions
    pub fn boxed(self) -> StatefulAction<Box<dyn Action>>
    where
        Self: 'static,
    {
        StatefulAction {
            action: Box::new(self.action),
            state: self.state,
        }
    }

    pub fn uncompleted(action: A) -> Self {
        Self {
            state: ActionState::Uncompleted,
            action,
        }
    }

    pub fn completed(action: A) -> Self {
        Self {
            state: ActionState::Completed,
            action,
        }
    }

    pub fn skipped(action: A) -> Self {
        Self {
            state: ActionState::Skipped,
            action,
        }
    }
}

impl<A> From<A> for StatefulAction<A>
where
    A: Action,
{
    fn from(action: A) -> Self {
        Self::uncompleted(action)
    }
}

fn describe_execute(action: &dyn Action, state: ActionState) -> Vec<ActionDescription> {
    match state {
        ActionState::Completed | ActionState::Skipped => vec![],
        _ => action.execute_description(),
    }
}

fn describe_revert(action: &dyn Action, state: ActionState) -> Vec<ActionDescription> {
    match state {
        ActionState::Uncompleted | ActionState::Skipped => vec![],
        _ => action.revert_description(),
    }
}

async fn try_execute(action: &mut dyn Action, state: &mut ActionState) -> anyhow::Result<()> {
    let span = action.tracing_span();
    let synopsis = action.tracing_synopsis();
    match *state {
        ActionState::Completed => {
            tracing::trace!(parent: &span, "Already done: {synopsis}");
            Ok(())
        },
        ActionState::Skipped => {
            tracing::trace!(parent: &span, "Skipped: {synopsis}");
            Ok(())
        },
        _ => {
            *state = ActionState::Progress;
            tracing::debug!(parent: &span, "Executing: {synopsis}");
            action
                .execute()
                .instrument(span.clone())
                .await
                .with_context(|| format!("{synopsis} failed"))?;
            *state = ActionState::Completed;
            tracing::debug!(parent: &span, "Completed: {synopsis}");
            Ok(())
        },
    }
}

async fn try_revert(action: &mut dyn Action, state: &mut ActionState) -> anyhow::Result<()> {
    let span = action.tracing_span();
    let synopsis = action.tracing_synopsis();
    match *state {
        ActionState::Uncompleted => {
            tracing::trace!(parent: &span, "Nothing to revert: {synopsis}");
            Ok(())
        },
        ActionState::Skipped => {
            tracing::trace!(parent: &span, "Skipped: {synopsis}");
            Ok(())
        },
        _ => {
            *state = ActionState::Progress;
            tracing::debug!(parent: &span, "Reverting: {synopsis}");
            action
                .revert()
                .instrument(span.clone())
                .await
                .with_context(|| format!("Reverting `{synopsis}` failed"))?;
            *state = ActionState::Uncompleted;
            tracing::debug!(parent: &span, "Reverted: {synopsis}");
            Ok(())
        },
    }
}
