# installer

A CLI installer framework: reversible actions, planners, receipts, and `plan` / `install` / `uninstall`.

Three concepts carry an installer built on this crate:

- **`Action`** — one executable, revertable step (`CreateDirectory`, `InstallBinary`, `StartSystemdUnit`). An action which can tell *while planning* that its work is done says so and is skipped.
- **`InstallPlan`** — the ordered sequence of actions, plus the planner and version that produced it. Shown for confirmation, written as the receipt.
- **`Planner`** — produces the plan, and holds its settings.

A consuming binary supplies product-specific planners and actions, then drives the CLI helpers.

The consuming crate owns clap (so planner subcommands stay product-specific) and calls `cli::subcommand::{install,plan,uninstall}::run`. Receipts live wherever [`Planner::receipt_path`](planner::Planner::receipt_path) says. See `src/bin/example-installer.rs`.

## Secrets

[`Secret`](secret::Secret) serializes as `"<redacted>"` and its `Debug` output is redacted too. Nothing in a revert needs the value. Files which hold secrets should be created with [`util::write_atomic`], so they are never briefly readable by anyone else.

When the installer re-runs itself under `sudo`, variables matching `App::env_prefix` are carried across by name (`--preserve-env`), never as `KEY=VALUE` arguments `sudo` would log.

## What revert will not do

Reverting is for undoing a stage that failed or is being redone. Actions which own irreplaceable state should say so in their revert description and leave it alone. Read the uninstall plan (`--explain`) before confirming.

`AptInstall` purges only packages it installed (those missing at plan time) and then `autoremove --purge`. Packages that were already present stay.
