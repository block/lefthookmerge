use serde_yaml::Value;
use std::fs;
use std::path::Path;

use super::Adapter;

const HOOKS_DIR_NAMES: &[&str] = &[".hooks", "git-hooks"];

/// Adapter for conventional hooks directories in the repo root.
///
/// Detects `.hooks/` or `git-hooks/` (first match wins) and generates lefthook
/// commands for all scripts matching the hook name: the exact match
/// (e.g. `pre-commit`) plus any prefixed scripts (e.g. `pre-commit-checkstyle`).
pub struct HooksDirAdapter;

/// Return the first hooks directory name that exists as a directory under `root`.
fn find_hooks_dir(root: &Path) -> Option<&'static str> {
    HOOKS_DIR_NAMES
        .iter()
        .copied()
        .find(|name| root.join(name).is_dir())
}

/// Collect sorted filenames from `hooks_dir` that match `hook_name` exactly
/// or start with `{hook_name}-`.
fn matching_scripts(hooks_dir: &Path, hook_name: &str) -> Vec<String> {
    let prefix = format!("{hook_name}-");
    let Ok(entries) = fs::read_dir(hooks_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == hook_name || name.starts_with(&prefix) {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

impl Adapter for HooksDirAdapter {
    fn name(&self) -> &str {
        "hooks-dir"
    }

    fn detect(&self, root: &Path) -> bool {
        find_hooks_dir(root).is_some()
    }

    fn generate_config(&self, root: &Path, hook_name: &str) -> Option<Value> {
        let dir_name = find_hooks_dir(root)?;
        let hooks_dir = root.join(dir_name);
        let scripts = matching_scripts(&hooks_dir, hook_name);
        if scripts.is_empty() {
            return None;
        }

        let commands: Vec<String> = scripts
            .iter()
            .map(|script| {
                let cmd_name = if *script == hook_name {
                    "hooks-dir".to_string()
                } else {
                    let suffix = &script[hook_name.len() + 1..];
                    format!("hooks-dir-{suffix}")
                };
                format!("    {cmd_name}:\n      run: {dir_name}/{script}")
            })
            .collect();

        let yaml = format!("{hook_name}:\n  commands:\n{}\n", commands.join("\n"));
        serde_yaml::from_str(&yaml).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn adapter() -> HooksDirAdapter {
        HooksDirAdapter
    }

    #[test]
    fn test_detect_with_dot_hooks() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".hooks")).unwrap();
        assert!(adapter().detect(dir.path()));
    }

    #[test]
    fn test_detect_with_git_hooks() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("git-hooks")).unwrap();
        assert!(adapter().detect(dir.path()));
    }

    #[test]
    fn test_detect_without_hooks_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!adapter().detect(dir.path()));
    }

    #[test]
    fn test_detect_file_not_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".hooks"), "not a directory").unwrap();
        assert!(!adapter().detect(dir.path()));
    }

    #[test]
    fn test_dot_hooks_takes_priority_over_git_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let dot_hooks = dir.path().join(".hooks");
        let git_hooks = dir.path().join("git-hooks");
        fs::create_dir_all(&dot_hooks).unwrap();
        fs::create_dir_all(&git_hooks).unwrap();
        fs::write(dot_hooks.join("pre-commit"), "#!/bin/sh\n").unwrap();
        fs::write(git_hooks.join("pre-commit"), "#!/bin/sh\n").unwrap();

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains(".hooks/pre-commit"), "uses .hooks: {out}");
        assert!(
            !out.contains("git-hooks/pre-commit"),
            "does not use git-hooks: {out}"
        );
    }

    #[test]
    fn test_generate_config_with_hook_script() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("pre-commit:"), "has hook key: {out}");
        assert!(out.contains(".hooks/pre-commit"), "has run command: {out}");
    }

    #[test]
    fn test_generate_config_git_hooks_dir() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join("git-hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("pre-commit:"), "has hook key: {out}");
        assert!(
            out.contains("git-hooks/pre-commit"),
            "has run command: {out}"
        );
    }

    #[test]
    fn test_generate_config_without_hook_script() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".hooks");
        fs::create_dir_all(&hooks_dir).unwrap();

        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_none()
        );
    }

    #[test]
    fn test_generate_config_different_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\necho push\n").unwrap();
        fs::write(hooks_dir.join("commit-msg"), "#!/bin/sh\necho msg\n").unwrap();

        let push_config = adapter().generate_config(dir.path(), "pre-push").unwrap();
        let out = serde_yaml::to_string(&push_config).unwrap();
        assert!(out.contains("pre-push:"), "has hook key: {out}");
        assert!(out.contains(".hooks/pre-push"), "has run command: {out}");

        let msg_config = adapter().generate_config(dir.path(), "commit-msg").unwrap();
        let out = serde_yaml::to_string(&msg_config).unwrap();
        assert!(out.contains("commit-msg:"), "has hook key: {out}");
        assert!(out.contains(".hooks/commit-msg"), "has run command: {out}");

        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_none()
        );
    }

    #[test]
    fn test_generate_config_with_prefixed_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\n").unwrap();
        fs::write(hooks_dir.join("pre-commit-checkstyle"), "#!/bin/sh\n").unwrap();
        fs::write(hooks_dir.join("pre-commit-detekt"), "#!/bin/sh\n").unwrap();
        // Should NOT be picked up for pre-commit
        fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\n").unwrap();
        fs::write(hooks_dir.join("pre-push-detekt"), "#!/bin/sh\n").unwrap();

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("hooks-dir:"), "has exact match cmd: {out}");
        assert!(
            out.contains("hooks-dir-checkstyle:"),
            "has checkstyle cmd: {out}"
        );
        assert!(out.contains("hooks-dir-detekt:"), "has detekt cmd: {out}");
        assert!(
            out.contains(".hooks/pre-commit-checkstyle"),
            "has checkstyle run: {out}"
        );
        assert!(
            out.contains(".hooks/pre-commit-detekt"),
            "has detekt run: {out}"
        );
        assert!(
            !out.contains("pre-push"),
            "should not contain pre-push: {out}"
        );
    }

    #[test]
    fn test_generate_config_prefixed_only_no_exact_match() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-commit-ktlint"), "#!/bin/sh\n").unwrap();

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("pre-commit:"), "has hook key: {out}");
        assert!(out.contains("hooks-dir-ktlint:"), "has ktlint cmd: {out}");
        assert!(
            out.contains(".hooks/pre-commit-ktlint"),
            "has ktlint run: {out}"
        );
        assert!(
            !out.contains("hooks-dir:\n"),
            "should not have exact match cmd: {out}"
        );
    }

    #[test]
    fn test_generate_config_prefixed_scripts_in_git_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join("git-hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\n").unwrap();
        fs::write(hooks_dir.join("pre-push-detekt"), "#!/bin/sh\n").unwrap();

        let config = adapter().generate_config(dir.path(), "pre-push").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("hooks-dir:"), "has exact match cmd: {out}");
        assert!(out.contains("hooks-dir-detekt:"), "has detekt cmd: {out}");
        assert!(
            out.contains("git-hooks/pre-push"),
            "uses git-hooks path: {out}"
        );
        assert!(
            out.contains("git-hooks/pre-push-detekt"),
            "uses git-hooks path for prefixed: {out}"
        );
    }

    #[test]
    fn test_matching_scripts_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-commit-zzz"), "#!/bin/sh\n").unwrap();
        fs::write(hooks_dir.join("pre-commit-aaa"), "#!/bin/sh\n").unwrap();
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\n").unwrap();

        let scripts = matching_scripts(&hooks_dir, "pre-commit");
        assert_eq!(
            scripts,
            vec!["pre-commit", "pre-commit-aaa", "pre-commit-zzz"]
        );
    }

    #[test]
    fn test_matching_scripts_ignores_directories() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\n").unwrap();
        fs::create_dir_all(hooks_dir.join("pre-commit-subdir")).unwrap();

        let scripts = matching_scripts(&hooks_dir, "pre-commit");
        assert_eq!(scripts, vec!["pre-commit"]);
    }
}
