use crate::cli::CleanAction;
use std::path::PathBuf;
use tachi_clean::sweep::SweepOptions;
use tachi_clean::tachi_clean::TachiCleanOptions;
use tachi_clean::target_clean::TargetCleanOptions;
use tachi_clean::wt_clean::{OutputFormat, WtRemoveOptions};

pub(crate) async fn run_clean_command(
    action: CleanAction,
) -> Result<(), Box<dyn std::error::Error>> {
    run_clean_command_sync(action).map_err(|err| err.into())
}

fn output_format(json: bool) -> OutputFormat {
    if json {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    }
}

fn run_clean_command_sync(action: CleanAction) -> Result<(), String> {
    match action {
        CleanAction::Target {
            path,
            force,
            dry_run: _,
            json,
        } => tachi_clean::target_clean::run_target_clean(TargetCleanOptions {
            path: path.unwrap_or_else(|| PathBuf::from(".")),
            force,
            output: output_format(json),
        }),
        CleanAction::Worktree {
            path,
            force,
            dry_run: _,
            json,
        } => tachi_clean::wt_clean::run_wt_remove(WtRemoveOptions {
            path,
            force,
            output: output_format(json),
        }),
        CleanAction::Sweep {
            root,
            max_age_days,
            force,
            dry_run: _,
            json,
        } => tachi_clean::sweep::run_sweep(SweepOptions {
            roots: root,
            max_age_days,
            force,
            output: output_format(json),
        }),
        CleanAction::Tachi {
            home,
            force,
            dry_run: _,
            json,
        } => tachi_clean::tachi_clean::run_tachi_clean(TachiCleanOptions {
            home,
            force,
            output: output_format(json),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_target_defaults_to_dry_run_and_json_output() {
        let root = unique_temp_dir("tachi-clean-cli-target");
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/test-bin"), "debug").unwrap();

        run_clean_command_sync(CleanAction::Target {
            path: Some(root.clone()),
            force: false,
            dry_run: false,
            json: true,
        })
        .unwrap();

        assert!(root.join("target/debug/test-bin").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clean_target_force_removes_debug_artifacts() {
        let root = unique_temp_dir("tachi-clean-cli-target-force");
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/test-bin"), "debug").unwrap();

        run_clean_command_sync(CleanAction::Target {
            path: Some(root.clone()),
            force: true,
            dry_run: false,
            json: true,
        })
        .unwrap();

        assert!(!root.join("target/debug").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
