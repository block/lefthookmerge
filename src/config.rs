use log::{debug, info};
use serde_yaml::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

/// Overrides for the global and local (repo) config paths.
/// CLI flags take precedence; env vars (`LHM_GLOBAL_CONFIG`, `LHM_LOCAL_CONFIG`)
/// are used as fallback so that overrides work during hook invocations too.
#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub global_config: Option<PathBuf>,
    pub local_config: Option<PathBuf>,
}

impl ConfigOverrides {
    /// Build overrides from CLI flags, falling back to env vars.
    pub fn new(global_flag: Option<PathBuf>, local_flag: Option<PathBuf>) -> Self {
        Self {
            global_config: global_flag.or_else(|| env::var("LHM_GLOBAL_CONFIG").ok().map(PathBuf::from)),
            local_config: local_flag.or_else(|| env::var("LHM_LOCAL_CONFIG").ok().map(PathBuf::from)),
        }
    }

    /// Build overrides from env vars only (used during hook invocations).
    pub fn from_env() -> Self {
        Self::new(None, None)
    }
}

pub const LEFTHOOK_EXTENSIONS: &[&str] = &["yml", "yaml", "json", "jsonc", "toml"];

pub const DEFAULT_GLOBAL_CONFIG: &str = r#"# Global lefthook configuration
output:
  - success
  - failure
pre-push:
  parallel: true
  commands:
    test:
      run: just test
      skip:
        - run: "! just --dry-run test"
    lint:
      run: just lint
      skip:
        - run: "! just --dry-run lint"
pre-commit:
  commands:
    fmt:
      stage_fixed: true
      run: just fmt
      skip:
        - run: "! just --dry-run fmt"
"#;

/// Search for a lefthook config file in the given directory.
/// Checks `lefthook.<ext>`, `.lefthook.<ext>`, and optionally `.config/lefthook.<ext>`.
pub fn find_config(dir: &Path, check_dot_config: bool) -> Option<PathBuf> {
    for ext in LEFTHOOK_EXTENSIONS {
        let candidates = if check_dot_config {
            vec![
                dir.join(format!("lefthook.{ext}")),
                dir.join(format!(".lefthook.{ext}")),
                dir.join(format!(".config/lefthook.{ext}")),
            ]
        } else {
            vec![
                dir.join(format!("lefthook.{ext}")),
                dir.join(format!(".lefthook.{ext}")),
            ]
        };
        for candidate in candidates {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn global_config(home: &Path, overrides: &ConfigOverrides) -> Option<PathBuf> {
    if let Some(ref p) = overrides.global_config {
        debug!("using global config override: {}", p.display());
        return Some(p.clone());
    }
    find_config(home, false)
}

pub fn repo_config(root: &Path, overrides: &ConfigOverrides) -> Option<PathBuf> {
    if let Some(ref p) = overrides.local_config {
        debug!("using local config override: {}", p.display());
        return Some(p.clone());
    }
    find_config(root, true)
}

/// Write the default global config to `~/.lefthook.yaml` if no global config exists.
pub fn install_default_global_config(home: &Path) -> Result<(), String> {
    if find_config(home, false).is_some() {
        debug!("global config already exists, skipping default");
        return Ok(());
    }
    let path = home.join(".lefthook.yaml");
    fs::write(&path, DEFAULT_GLOBAL_CONFIG).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    info!("created default global config at {}", path.display());
    Ok(())
}

/// Load the global config from `~/.lefthook.yaml` (or override) if it exists.
pub fn load_global_config(home: &Path, overrides: &ConfigOverrides) -> Result<Option<Value>, String> {
    match global_config(home, overrides) {
        Some(path) => read_yaml(&path).map(Some),
        None => {
            debug!("no global config file found");
            Ok(None)
        }
    }
}

pub fn read_yaml(path: &Path) -> Result<Value, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

/// Serialize a merged config value to a temp file for lefthook.
pub fn write_merged_temp(merged: Value) -> Result<NamedTempFile, String> {
    let content = serde_yaml::to_string(&merged).map_err(|e| format!("failed to serialize config: {e}"))?;
    debug!("merged config:\n{content}");

    let mut tmp = tempfile::Builder::new()
        .suffix(".yml")
        .tempfile()
        .map_err(|e| format!("failed to create temp file: {e}"))?;
    write!(tmp, "{content}").map_err(|e| format!("failed to write temp config: {e}"))?;
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn no_overrides() -> ConfigOverrides {
        ConfigOverrides::default()
    }

    fn to_yaml(v: &Value) -> String {
        serde_yaml::to_string(v).unwrap()
    }

    #[test]
    fn test_find_config_yaml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lefthook.yaml"), "").unwrap();
        assert_eq!(find_config(dir.path(), false), Some(dir.path().join("lefthook.yaml")));
    }

    #[test]
    fn test_find_config_yml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lefthook.yml"), "").unwrap();
        assert_eq!(find_config(dir.path(), false), Some(dir.path().join("lefthook.yml")));
    }

    #[test]
    fn test_find_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lefthook.toml"), "").unwrap();
        assert_eq!(find_config(dir.path(), false), Some(dir.path().join("lefthook.toml")));
    }

    #[test]
    fn test_find_config_dotted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".lefthook.json"), "").unwrap();
        assert_eq!(find_config(dir.path(), false), Some(dir.path().join(".lefthook.json")));
    }

    #[test]
    fn test_find_config_dot_config_subdir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".config")).unwrap();
        fs::write(dir.path().join(".config/lefthook.toml"), "").unwrap();
        assert_eq!(
            find_config(dir.path(), true),
            Some(dir.path().join(".config/lefthook.toml"))
        );
        // Should not find .config/ variant when check_dot_config is false
        assert_eq!(find_config(dir.path(), false), None);
    }

    #[test]
    fn test_find_config_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_config(dir.path(), true), None);
    }

    #[test]
    fn test_find_config_prefers_yml_over_yaml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lefthook.yml"), "").unwrap();
        fs::write(dir.path().join("lefthook.yaml"), "").unwrap();
        // yml comes first in LEFTHOOK_EXTENSIONS
        assert_eq!(find_config(dir.path(), false), Some(dir.path().join("lefthook.yml")));
    }

    #[test]
    fn test_install_default_global_config_creates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        install_default_global_config(dir.path()).unwrap();

        let created = dir.path().join(".lefthook.yaml");
        assert!(created.is_file());
        let content = fs::read_to_string(&created).unwrap();
        assert!(content.contains("pre-push:"));
        assert!(content.contains("pre-commit:"));
    }

    #[test]
    fn test_install_default_global_config_skips_when_exists() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("lefthook.yml");
        fs::write(&existing, "custom: true\n").unwrap();

        install_default_global_config(dir.path()).unwrap();

        // Original file untouched
        assert_eq!(fs::read_to_string(&existing).unwrap(), "custom: true\n");
        // No .lefthook.yaml created
        assert!(!dir.path().join(".lefthook.yaml").exists());
    }

    #[test]
    fn test_load_global_config_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_global_config(dir.path(), &no_overrides()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_load_global_config_returns_some_when_exists() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".lefthook.yaml"),
            "pre-commit:\n  commands:\n    fmt:\n      run: echo hi\n",
        )
        .unwrap();
        let result = load_global_config(dir.path(), &no_overrides()).unwrap();
        assert!(result.is_some());
        let out = to_yaml(&result.unwrap());
        assert!(out.contains("pre-commit:"));
    }

    #[test]
    fn test_global_config_override() {
        let dir = tempfile::tempdir().unwrap();
        let override_path = dir.path().join("custom-global.yaml");
        fs::write(&override_path, "pre-push:\n  commands:\n    t:\n      run: echo t\n").unwrap();

        let overrides = ConfigOverrides {
            global_config: Some(override_path.clone()),
            local_config: None,
        };
        let result = global_config(dir.path(), &overrides);
        assert_eq!(result, Some(override_path));
    }

    #[test]
    fn test_repo_config_override() {
        let dir = tempfile::tempdir().unwrap();
        let override_path = dir.path().join("custom-local.yaml");
        fs::write(&override_path, "pre-commit:\n  commands:\n    f:\n      run: echo f\n").unwrap();

        let overrides = ConfigOverrides {
            global_config: None,
            local_config: Some(override_path.clone()),
        };
        let result = repo_config(dir.path(), &overrides);
        assert_eq!(result, Some(override_path));
    }

    #[test]
    fn test_overrides_from_env() {
        let _guard1 = TempEnvVar::set("LHM_GLOBAL_CONFIG", "/tmp/g.yaml");
        let _guard2 = TempEnvVar::set("LHM_LOCAL_CONFIG", "/tmp/l.yaml");

        let o = ConfigOverrides::from_env();
        assert_eq!(o.global_config, Some(PathBuf::from("/tmp/g.yaml")));
        assert_eq!(o.local_config, Some(PathBuf::from("/tmp/l.yaml")));
    }

    #[test]
    fn test_cli_flags_override_env_vars() {
        let _guard = TempEnvVar::set("LHM_GLOBAL_CONFIG", "/tmp/env.yaml");

        let o = ConfigOverrides::new(Some(PathBuf::from("/tmp/cli.yaml")), None);
        assert_eq!(o.global_config, Some(PathBuf::from("/tmp/cli.yaml")));
    }

    /// RAII guard that sets an env var and restores the previous value on drop.
    struct TempEnvVar {
        key: String,
        prev: Option<String>,
    }

    impl TempEnvVar {
        fn set(key: &str, val: &str) -> Self {
            let prev = env::var(key).ok();
            // SAFETY: test-only, tests using this helper run sequentially
            unsafe { env::set_var(key, val) };
            Self {
                key: key.to_string(),
                prev,
            }
        }
    }

    impl Drop for TempEnvVar {
        fn drop(&mut self) {
            // SAFETY: test-only, restoring previous env state
            match &self.prev {
                Some(v) => unsafe { env::set_var(&self.key, v) },
                None => unsafe { env::remove_var(&self.key) },
            }
        }
    }
}
