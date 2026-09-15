/*! A CLI installer framework

Three concepts carry an installer built on this crate:

* [`Action`]: one executable, revertable step, possibly orchestrating sub-[`Action`]s.
* [`InstallPlan`]: the ordered sequence of actions, plus the planner and version that
  produced it. It is what gets shown for confirmation, and what is written out as the
  receipt.
* [`Planner`](planner::Planner): produces the plan, and holds its settings.

A consuming binary supplies its own planners and product-specific actions, then drives
[`cli`] for `plan` / `install` / `uninstall`.
*/

pub mod action;
pub mod cli;
pub mod command;
mod plan;
pub mod planner;
pub mod secret;
pub mod util;

pub use action::Action;
pub use plan::InstallPlan;
pub use secret::Secret;
