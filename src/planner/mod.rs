/*! [`Planner`]s: what produces an [`InstallPlan`](crate::InstallPlan)

A consuming installer supplies its own planners. Each planner holds the settings for one
kind of install, and [`plan`](Planner::plan) is how it turns those settings into a sequence
of [`Action`]s.

[`pre_install_check`](Planner::pre_install_check) is for what must be true of the machine
before anything runs. [`receipt_path`](Planner::receipt_path) is where the plan is written
after an install (or a failed one), so `uninstall` knows exactly what was done.
*/

use std::collections::HashMap;
use std::path::PathBuf;

use crate::{action::StatefulAction, Action};

/// Something which can produce an [`InstallPlan`](crate::InstallPlan)
#[async_trait::async_trait]
#[typetag::serde(tag = "planner")]
pub trait Planner: std::fmt::Debug + Send + Sync + dyn_clone::DynClone {
    /// Instantiate the planner with default settings, if possible
    async fn default() -> anyhow::Result<Self>
    where
        Self: Sized;

    /// Plan the [`Action`]s this install needs
    async fn plan(&self) -> anyhow::Result<Vec<StatefulAction<Box<dyn Action>>>>;

    /// Every setting in force, for `--explain` and for the receipt
    fn settings(&self) -> anyhow::Result<HashMap<String, serde_json::Value>>;

    /// Only the settings which differ from the defaults, for the plan description
    async fn configured_settings(&self) -> anyhow::Result<HashMap<String, serde_json::Value>>;

    /// Where this plan's receipt is written
    fn receipt_path(&self) -> PathBuf;

    fn boxed(self) -> Box<dyn Planner>
    where
        Self: Sized + 'static,
    {
        Box::new(self)
    }

    /// Whether this planner can run on this machine at all
    async fn platform_check(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// The gates which must hold before this install may run
    async fn pre_install_check(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn pre_uninstall_check(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

dyn_clone::clone_trait_object!(Planner);

/// Only the settings which differ from this planner type's defaults
pub async fn diff_from_default<P>(planner: &P) -> anyhow::Result<HashMap<String, serde_json::Value>>
where
    P: Planner + Sized,
{
    let default = P::default().await?.settings()?;
    let configured = planner.settings()?;

    let mut settings = HashMap::new();
    for (key, value) in configured.into_iter() {
        if default.get(&key) != Some(&value) {
            settings.insert(key, value);
        }
    }

    Ok(settings)
}
