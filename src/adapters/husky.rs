use serde_yaml::Value;
use std::path::Path;

use super::Adapter;

/// Adapter for the [husky](https://typicode.github.io/husky/) hook manager.
///
/// Detects a `.husky/` directory in the repo root and generates a lefthook
/// command that executes `.husky/<hook>` if the corresponding script exists.
pub struct HuskyAdapter;

impl Adapter for HuskyAdapter {
    fn name(&self) -> &str {
        "husky"
    }

    fn detect(&self, root: &Path) -> bool {
        root.join(".husky").is_dir()
    }

    fn generate_config(&self, root: &Path, hook_name: &str) -> Option<Value> {
        let script = root.join(".husky").join(hook_name);
        if !script.is_file() {
            return None;
        }

        let config =
            format!("{hook_name}:\n  commands:\n    husky:\n      run: .husky/{hook_name}\n");
        serde_yaml::from_str(&config).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn adapter() -> HuskyAdapter {
        HuskyAdapter
    }

    #[test]
    fn test_detect_with_husky_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".husky")).unwrap();
        assert!(adapter().detect(dir.path()));
    }

    #[test]
    fn test_detect_without_husky_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!adapter().detect(dir.path()));
    }

    #[test]
    fn test_detect_file_not_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".husky"), "not a directory").unwrap();
        assert!(!adapter().detect(dir.path()));
    }

    #[test]
    fn test_generate_config_with_hook_script() {
        let dir = tempfile::tempdir().unwrap();
        let husky_dir = dir.path().join(".husky");
        fs::create_dir_all(&husky_dir).unwrap();
        fs::write(husky_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("pre-commit:"), "has hook key: {out}");
        assert!(out.contains(".husky/pre-commit"), "has run command: {out}");
    }

    #[test]
    fn test_generate_config_without_hook_script() {
        let dir = tempfile::tempdir().unwrap();
        let husky_dir = dir.path().join(".husky");
        fs::create_dir_all(&husky_dir).unwrap();

        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_none()
        );
    }

    #[test]
    fn test_generate_config_different_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let husky_dir = dir.path().join(".husky");
        fs::create_dir_all(&husky_dir).unwrap();
        fs::write(husky_dir.join("pre-push"), "#!/bin/sh\necho push\n").unwrap();
        fs::write(husky_dir.join("commit-msg"), "#!/bin/sh\necho msg\n").unwrap();

        let push_config = adapter().generate_config(dir.path(), "pre-push").unwrap();
        let out = serde_yaml::to_string(&push_config).unwrap();
        assert!(out.contains("pre-push:"), "has hook key: {out}");
        assert!(out.contains(".husky/pre-push"), "has run command: {out}");

        let msg_config = adapter().generate_config(dir.path(), "commit-msg").unwrap();
        let out = serde_yaml::to_string(&msg_config).unwrap();
        assert!(out.contains("commit-msg:"), "has hook key: {out}");
        assert!(out.contains(".husky/commit-msg"), "has run command: {out}");

        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_none()
        );
    }
}
