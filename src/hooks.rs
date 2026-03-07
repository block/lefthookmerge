use log::debug;
use serde_yaml::Value;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

pub const GIT_HOOKS: &[&str] = &[
    "applypatch-msg",
    "commit-msg",
    "post-applypatch",
    "post-checkout",
    "post-commit",
    "post-merge",
    "post-rewrite",
    "pre-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "pre-push",
    "pre-rebase",
    "prepare-commit-msg",
];

pub fn is_hook_name(name: &str) -> bool {
    GIT_HOOKS.contains(&name)
}

/// Hooks where commands mutate shared state and must not run in parallel.
/// - `pre-commit` / `pre-merge-commit`: formatters mutate the working tree/index
/// - `prepare-commit-msg` / `commit-msg` / `applypatch-msg`: edit a single message file
const SERIAL_HOOKS: &[&str] = &[
    "applypatch-msg",
    "commit-msg",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
];

pub fn create_hook_symlinks(dir: &Path, binary: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;

    remove_stale_hooks(dir);

    for hook in GIT_HOOKS {
        let link = dir.join(hook);
        let _ = fs::remove_file(&link);
        symlink(binary, &link).map_err(|e| format!("failed to symlink {}: {e}", link.display()))?;
    }
    Ok(())
}

/// Remove any entries in the hooks dir that aren't in the current `GIT_HOOKS` list.
fn remove_stale_hooks(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !GIT_HOOKS.contains(&name_str) {
            debug!("removing stale hook: {name_str}");
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Annotate adapter-generated config with lefthook settings:
/// - `parallel: true` on hooks that don't mutate shared state
/// - `stage_fixed: true` on each command within `pre-commit` and `pre-merge-commit` hooks
pub fn annotate_hooks(config: Value) -> Value {
    let Value::Mapping(mut root) = config else {
        return config;
    };
    for (key, val) in &mut root {
        if let (Some(name), Value::Mapping(hook_map)) = (key.as_str(), val)
            && is_hook_name(name)
        {
            if !SERIAL_HOOKS.contains(&name) {
                hook_map.insert(Value::String("parallel".to_string()), Value::Bool(true));
            }
            if name == "pre-commit" || name == "pre-merge-commit" {
                set_stage_fixed(hook_map);
            }
        }
    }
    Value::Mapping(root)
}

/// Add `stage_fixed: true` to every command in a hook mapping.
fn set_stage_fixed(hook_map: &mut serde_yaml::Mapping) {
    let commands_key = Value::String("commands".to_string());
    if let Some(Value::Mapping(commands)) = hook_map.get_mut(&commands_key) {
        for (_cmd_name, cmd_val) in commands.iter_mut() {
            if let Value::Mapping(cmd_map) = cmd_val {
                cmd_map.insert(Value::String("stage_fixed".to_string()), Value::Bool(true));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn yaml(s: &str) -> Value {
        serde_yaml::from_str(s).unwrap()
    }

    fn to_yaml(v: &Value) -> String {
        serde_yaml::to_string(v).unwrap()
    }

    #[test]
    fn test_is_hook_name() {
        assert!(is_hook_name("pre-commit"));
        assert!(is_hook_name("commit-msg"));
        assert!(is_hook_name("pre-push"));
        assert!(is_hook_name("post-merge"));
        assert!(!is_hook_name("lhm"));
        assert!(!is_hook_name("cargo"));
        assert!(!is_hook_name(""));
    }

    #[test]
    fn test_create_hook_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");
        let fake_binary = dir.path().join("lhm");
        fs::write(&fake_binary, "fake").unwrap();

        create_hook_symlinks(&hooks, &fake_binary).unwrap();

        for hook in GIT_HOOKS {
            let link = hooks.join(hook);
            assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
            assert_eq!(fs::read_link(&link).unwrap(), fake_binary);
        }
    }

    #[test]
    fn test_create_hook_symlinks_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");
        fs::create_dir_all(&hooks).unwrap();

        // Create a pre-existing file where a symlink will go
        fs::write(hooks.join("pre-commit"), "old").unwrap();

        let fake_binary = dir.path().join("lhm");
        fs::write(&fake_binary, "fake").unwrap();

        create_hook_symlinks(&hooks, &fake_binary).unwrap();

        let link = hooks.join("pre-commit");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), fake_binary);
    }

    #[test]
    fn test_create_hook_symlinks_removes_stale_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join("hooks");
        fs::create_dir_all(&hooks).unwrap();

        // Create hooks that are no longer in GIT_HOOKS
        let stale = ["reference-transaction", "fsmonitor-watchman", "update"];
        for name in &stale {
            fs::write(hooks.join(name), "old").unwrap();
        }

        let fake_binary = dir.path().join("lhm");
        fs::write(&fake_binary, "fake").unwrap();

        create_hook_symlinks(&hooks, &fake_binary).unwrap();

        for name in &stale {
            assert!(
                !hooks.join(name).exists(),
                "stale hook {name} should be removed"
            );
        }
        for hook in GIT_HOOKS {
            assert!(
                hooks
                    .join(hook)
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[test]
    fn test_annotate_hooks_parallel_on_safe_hooks() {
        let config =
            yaml("pre-push:\n  commands:\n    foo:\n      run: echo hi\noutput:\n  - success\n");
        let result = annotate_hooks(config);
        let out = to_yaml(&result);
        assert!(out.contains("parallel: true"), "injects parallel: {out}");
        assert!(out.contains("output:"), "non-hook keys preserved: {out}");
    }

    #[test]
    fn test_annotate_hooks_no_parallel_on_serial_hooks() {
        for hook in SERIAL_HOOKS {
            let config = yaml(&format!(
                "{hook}:\n  commands:\n    foo:\n      run: echo hi\n"
            ));
            let result = annotate_hooks(config);
            let out = to_yaml(&result);
            assert!(!out.contains("parallel"), "no parallel on {hook}: {out}");
        }
    }

    #[test]
    fn test_annotate_hooks_stage_fixed_on_pre_commit_hooks() {
        for hook in &["pre-commit", "pre-merge-commit"] {
            let config = yaml(&format!(
                "{hook}:\n  commands:\n    foo:\n      run: echo hi\n    bar:\n      run: echo bye\n"
            ));
            let result = annotate_hooks(config);
            let out = to_yaml(&result);
            assert!(
                out.contains("stage_fixed: true"),
                "injects stage_fixed on {hook}: {out}"
            );
            assert!(!out.contains("parallel"), "no parallel on {hook}: {out}");
            assert_eq!(
                out.matches("stage_fixed").count(),
                2,
                "both commands get stage_fixed on {hook}: {out}"
            );
        }
    }

    #[test]
    fn test_annotate_hooks_no_stage_fixed_on_pre_push() {
        let config = yaml("pre-push:\n  commands:\n    foo:\n      run: echo hi\n");
        let result = annotate_hooks(config);
        let out = to_yaml(&result);
        assert!(
            !out.contains("stage_fixed"),
            "no stage_fixed on pre-push: {out}"
        );
    }

    #[test]
    fn test_annotate_hooks_skips_non_hook_keys() {
        let config = yaml("output:\n  - success\n");
        let result = annotate_hooks(config);
        let out = to_yaml(&result);
        assert!(!out.contains("parallel"), "no parallel on non-hook: {out}");
    }
}
