use std::{collections::HashMap, path::PathBuf};

use crate::{
    action::base::{CreateDirectory, CreateFile, CreateSymlink, RequirePaths},
    action::{fold_errors, Action, ActionState, StatefulAction},
    planner::{diff_from_default, Planner},
    InstallPlan,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TestPlanner {
    root: PathBuf,
}

#[async_trait::async_trait]
#[typetag::serde(name = "test")]
impl Planner for TestPlanner {
    async fn default() -> anyhow::Result<Self> {
        Ok(Self {
            root: PathBuf::from("/tmp/installer-test"),
        })
    }

    async fn plan(&self) -> anyhow::Result<Vec<StatefulAction<Box<dyn Action>>>> {
        Ok(vec![
            CreateDirectory::plan(self.root.join("bin"), None, None, Some(0o755), false)
                .await?
                .boxed(),
            CreateFile::plan(
                self.root.join("bin").join("README"),
                None,
                None,
                Some(0o644),
                "ok\n".into(),
                false,
            )
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

#[tokio::test]
async fn plan_describes_actions() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let planner = TestPlanner {
        root: dir.path().to_path_buf(),
    };
    let plan = InstallPlan::plan(planner, "Example", "0.1.0").await?;
    let described = plan.describe_install(true).await?;
    assert!(described.contains("Example install plan (v0.1.0)"));
    assert!(described.contains("Create directory"));
    assert!(described.contains("Planner: test"));
    Ok(())
}

#[tokio::test]
async fn install_and_uninstall_round_trip() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().to_path_buf();
    let planner = TestPlanner { root: root.clone() };
    let mut plan = InstallPlan::plan(planner, "Example", env!("CARGO_PKG_VERSION")).await?;

    plan.install(None, env!("CARGO_PKG_VERSION")).await?;
    assert!(root.join("bin").is_dir());
    assert_eq!(
        tokio::fs::read_to_string(root.join("bin").join("README")).await?,
        "ok\n"
    );
    let receipt = tokio::fs::read_to_string(root.join("receipt.json")).await?;
    assert!(receipt.contains("\"product\": \"Example\""));

    let described = plan.describe_uninstall(false).await?;
    assert!(described.contains("Remove the directory"));

    plan.uninstall(None, env!("CARGO_PKG_VERSION")).await?;
    assert!(!root.join("bin").join("README").exists());
    Ok(())
}

#[tokio::test]
async fn receipt_round_trips() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let planner = TestPlanner {
        root: dir.path().to_path_buf(),
    };
    let plan = InstallPlan::plan(planner, "Example", env!("CARGO_PKG_VERSION")).await?;
    let json = serde_json::to_string(&plan)?;
    let restored: InstallPlan = serde_json::from_str(&json)?;
    restored.check_compatible(env!("CARGO_PKG_VERSION"))?;
    let err = restored.check_compatible("9.9.9").expect_err("mismatch");
    assert!(err.to_string().contains("9.9.9"));
    Ok(())
}

#[tokio::test]
async fn configured_settings_omit_defaults() -> anyhow::Result<()> {
    let default = TestPlanner::default().await?;
    assert!(default.configured_settings().await?.is_empty());

    let custom = TestPlanner {
        root: PathBuf::from("/elsewhere"),
    };
    let settings = custom.configured_settings().await?;
    assert_eq!(
        settings.get("root"),
        Some(&serde_json::Value::String("/elsewhere".into()))
    );
    Ok(())
}

#[tokio::test]
async fn skipped_action_is_a_noop_both_ways() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("keep");
    tokio::fs::create_dir(&path).await?;
    let mut action = CreateDirectory::plan(&path, None, None, None, true).await?;
    assert_eq!(action.state(), ActionState::Skipped);
    action.try_execute().await?;
    action.try_revert().await?;
    assert!(path.exists());
    Ok(())
}

#[tokio::test]
async fn completed_constructor_skips_execute() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("file");
    let planned = CreateFile::plan(&path, None, None, None, "x".into(), false).await?;
    let mut action = StatefulAction::completed(planned.inner().clone());
    action.try_execute().await?;
    assert!(!path.exists());
    Ok(())
}

#[tokio::test]
async fn require_paths_names_the_missing_file() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("need-me");
    let mut action = RequirePaths::plan(
        "the layout",
        vec![(missing.clone(), "the thing".into())],
        "run the previous stage",
    )
    .await?;
    let err = format!(
        "{:#}",
        action.try_execute().await.expect_err("missing path")
    );
    assert!(err.contains("the thing"));
    assert!(err.contains("run the previous stage"));
    Ok(())
}

#[tokio::test]
async fn symlink_create_and_revert() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("target");
    let link = dir.path().join("link");
    tokio::fs::write(&target, "data").await?;

    let mut action = CreateSymlink::plan(&link, &target, None).await?;
    action.try_execute().await?;
    assert_eq!(tokio::fs::read_link(&link).await?, target);

    let again = CreateSymlink::plan(&link, &target, None).await?;
    assert_eq!(again.state(), ActionState::Completed);

    action.try_revert().await?;
    assert!(!link.exists());
    Ok(())
}

#[test]
fn fold_errors_joins_several() {
    assert!(fold_errors(vec![]).is_ok());
    let one = fold_errors(vec![anyhow::anyhow!("a")]).expect_err("one");
    assert_eq!(format!("{one:#}"), "a");
    let many =
        fold_errors(vec![anyhow::anyhow!("a"), anyhow::anyhow!("b")]).expect_err("many");
    assert!(many.to_string().contains("2 steps failed"));
}
