mod hooks_dir;
mod husky;
mod pre_commit;

use serde_yaml::Value;
use std::path::Path;

pub use hooks_dir::HooksDirAdapter;
pub use husky::HuskyAdapter;
pub use pre_commit::PreCommitAdapter;

/// Adapter for translating third-party git hook managers into lefthook configs.
///
/// Each implementation detects a specific hook manager and generates a
/// lefthook-compatible YAML config fragment for a given hook name.
pub trait Adapter {
    /// Human-readable name of this adapter (e.g. "pre-commit", "husky").
    fn name(&self) -> &str;

    /// Returns `true` if this adapter's hook manager is present in the repo.
    fn detect(&self, root: &Path) -> bool;

    /// Generate a lefthook config `Value` for the given hook name.
    ///
    /// Returns `None` if this adapter has nothing to run for the given hook
    /// (e.g. no matching hook script exists).
    fn generate_config(&self, root: &Path, hook_name: &str) -> Option<Value>;
}

/// All known adapters, in priority order.
fn all_adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(PreCommitAdapter),
        Box::new(HuskyAdapter),
        Box::new(HooksDirAdapter),
    ]
}

/// Detect the first applicable adapter for the given repo root.
pub fn detect_adapter(root: &Path) -> Option<Box<dyn Adapter>> {
    all_adapters().into_iter().find(|a| a.detect(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_detect_adapter_pre_commit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".pre-commit-config.yaml"), "repos: []\n").unwrap();
        let adapter = detect_adapter(dir.path()).unwrap();
        assert_eq!(adapter.name(), "pre-commit");
    }

    #[test]
    fn test_detect_adapter_husky() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".husky")).unwrap();
        let adapter = detect_adapter(dir.path()).unwrap();
        assert_eq!(adapter.name(), "husky");
    }

    #[test]
    fn test_detect_adapter_hooks_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".hooks")).unwrap();
        let adapter = detect_adapter(dir.path()).unwrap();
        assert_eq!(adapter.name(), "hooks-dir");
    }

    #[test]
    fn test_detect_adapter_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_adapter(dir.path()).is_none());
    }

    #[test]
    fn test_detect_adapter_priority_pre_commit_over_husky() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".pre-commit-config.yaml"), "repos: []\n").unwrap();
        fs::create_dir_all(dir.path().join(".husky")).unwrap();
        let adapter = detect_adapter(dir.path()).unwrap();
        assert_eq!(adapter.name(), "pre-commit");
    }
}
