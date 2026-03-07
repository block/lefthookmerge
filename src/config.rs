use log::{debug, info};
use serde_yaml::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

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

pub fn global_config(home: &Path) -> Option<PathBuf> {
    find_config(home, false)
}

pub fn repo_config(root: &Path) -> Option<PathBuf> {
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

/// Load the global config from `~/.lefthook.yaml` if it exists.
pub fn load_global_config(home: &Path) -> Result<Option<Value>, String> {
    match global_config(home) {
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
        let result = load_global_config(dir.path()).unwrap();
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
        let result = load_global_config(dir.path()).unwrap();
        assert!(result.is_some());
        let out = to_yaml(&result.unwrap());
        assert!(out.contains("pre-commit:"));
    }
}
