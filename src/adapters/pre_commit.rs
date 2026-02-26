use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::fs;
use std::path::Path;

use super::Adapter;

/// Adapter for the [pre-commit](https://pre-commit.com/) hook manager.
///
/// Parses `.pre-commit-config.yaml` and translates `repo: local` hooks into
/// lefthook commands. Remote repos are skipped since their `entry` is defined
/// in the remote `.pre-commit-hooks.yaml` and can't be resolved without cloning.
pub struct PreCommitAdapter;

impl Adapter for PreCommitAdapter {
    fn name(&self) -> &str {
        "pre-commit"
    }

    fn detect(&self, root: &Path) -> bool {
        root.join(".pre-commit-config.yaml").is_file()
    }

    fn generate_config(&self, root: &Path, hook_name: &str) -> Option<Value> {
        let content = fs::read_to_string(root.join(".pre-commit-config.yaml")).ok()?;
        let config: PreCommitConfig = serde_yaml::from_str(&content).ok()?;

        let mut commands = Mapping::new();

        for repo in &config.repos {
            if repo.repo != "local" {
                continue;
            }
            for hook in &repo.hooks {
                if !hook_matches_stage(hook, &config.default_stages, hook_name) {
                    continue;
                }
                if let Some(cmd) = translate_hook(hook) {
                    commands.insert(str_val(&hook.id), Value::Mapping(cmd));
                }
            }
        }

        if commands.is_empty() {
            return None;
        }

        let mut hook_mapping = Mapping::new();
        hook_mapping.insert(str_val("commands"), Value::Mapping(commands));

        let mut root_mapping = Mapping::new();
        root_mapping.insert(str_val(hook_name), Value::Mapping(hook_mapping));

        Some(Value::Mapping(root_mapping))
    }
}

// ---------------------------------------------------------------------------
// .pre-commit-config.yaml schema (subset)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PreCommitConfig {
    #[serde(default)]
    repos: Vec<Repo>,
    #[serde(default)]
    default_stages: Vec<String>,
}

#[derive(Deserialize)]
struct Repo {
    repo: String,
    #[serde(default)]
    hooks: Vec<Hook>,
}

#[derive(Deserialize)]
struct Hook {
    id: String,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    stages: Vec<String>,
    #[serde(default)]
    files: Option<String>,
    #[serde(default)]
    exclude: Option<String>,
    #[serde(default = "default_true")]
    pass_filenames: bool,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    types_or: Vec<String>,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Translation helpers
// ---------------------------------------------------------------------------

/// Check whether a hook should run for the given git hook stage.
///
/// Falls back to `default_stages` when the hook has no explicit `stages`.
/// Empty stages (at both levels) means the hook runs for all stages.
fn hook_matches_stage(hook: &Hook, default_stages: &[String], hook_name: &str) -> bool {
    let stages = if hook.stages.is_empty() {
        default_stages
    } else {
        &hook.stages
    };
    stages.is_empty() || stages.iter().any(|s| s == hook_name)
}

/// Translate a single pre-commit hook into a lefthook command mapping.
///
/// Returns `None` if the hook has no `entry` (which happens for remote-repo
/// hooks that only specify `id`).
fn translate_hook(hook: &Hook) -> Option<Mapping> {
    let entry = hook.entry.as_deref()?;

    let mut run_parts = Vec::with_capacity(1 + hook.args.len() + 1);
    run_parts.push(entry.to_string());
    run_parts.extend(hook.args.iter().cloned());
    if hook.pass_filenames {
        run_parts.push("{staged_files}".to_string());
    }

    let mut cmd = Mapping::new();
    cmd.insert(str_val("run"), str_val(&run_parts.join(" ")));

    if let Some(ref files) = hook.files {
        cmd.insert(str_val("files"), str_val(files));
    }
    if let Some(ref exclude) = hook.exclude {
        cmd.insert(str_val("exclude"), str_val(exclude));
    }
    if let Some(glob) = types_to_glob(&hook.types, &hook.types_or) {
        cmd.insert(str_val("glob"), str_val(&glob));
    }

    Some(cmd)
}

/// Map pre-commit `types` / `types_or` to a lefthook `glob` pattern.
///
/// `types` uses AND logic (in practice usually a single file type).
/// `types_or` uses OR logic. Both are combined into one glob.
fn types_to_glob(types: &[String], types_or: &[String]) -> Option<String> {
    let mut extensions: Vec<&str> = Vec::new();

    for ty in types {
        if let Some(ext) = type_to_extensions(ty) {
            extensions.extend(ext.split(','));
        }
    }
    for ty in types_or {
        if let Some(ext) = type_to_extensions(ty) {
            extensions.extend(ext.split(','));
        }
    }

    extensions.sort_unstable();
    extensions.dedup();

    match extensions.len() {
        0 => None,
        1 => Some(format!("*.{}", extensions[0])),
        _ => Some(format!("*.{{{}}}", extensions.join(","))),
    }
}

/// Map a pre-commit file-type tag to one or more file extensions.
///
/// Returns comma-separated extensions for types that map to multiple
/// (e.g. `"yaml"` → `"yml,yaml"`). Returns `None` for non-extension
/// types like `file`, `text`, `executable`.
fn type_to_extensions(ty: &str) -> Option<&'static str> {
    match ty {
        "python" => Some("py"),
        "javascript" => Some("js"),
        "jsx" => Some("jsx"),
        "typescript" => Some("ts"),
        "tsx" => Some("tsx"),
        "ruby" => Some("rb"),
        "rust" => Some("rs"),
        "go" => Some("go"),
        "java" => Some("java"),
        "c" => Some("c"),
        "c++" | "cpp" => Some("cpp"),
        "c#" | "csharp" => Some("cs"),
        "yaml" => Some("yml,yaml"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "markdown" => Some("md"),
        "shell" | "bash" | "zsh" | "sh" => Some("sh"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "html" => Some("html"),
        "xml" => Some("xml"),
        "sql" => Some("sql"),
        "swift" => Some("swift"),
        "kotlin" => Some("kt"),
        "scala" => Some("scala"),
        "haskell" => Some("hs"),
        "lua" => Some("lua"),
        "perl" => Some("pl"),
        "php" => Some("php"),
        "r" => Some("R"),
        _ => None,
    }
}

fn str_val(s: &str) -> Value {
    Value::String(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> PreCommitAdapter {
        PreCommitAdapter
    }

    fn write_config(dir: &Path, content: &str) {
        fs::write(dir.join(".pre-commit-config.yaml"), content).unwrap();
    }

    // -- detection --

    #[test]
    fn test_detect_with_config() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "repos: []\n");
        assert!(adapter().detect(dir.path()));
    }

    #[test]
    fn test_detect_without_config() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!adapter().detect(dir.path()));
    }

    #[test]
    fn test_detect_directory_not_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".pre-commit-config.yaml")).unwrap();
        assert!(!adapter().detect(dir.path()));
    }

    // -- stage matching --

    #[test]
    fn test_hook_matches_stage_explicit() {
        let hook = Hook {
            id: "x".into(),
            entry: None,
            args: vec![],
            stages: vec!["pre-commit".into()],
            files: None,
            exclude: None,
            pass_filenames: true,
            types: vec![],
            types_or: vec![],
        };
        assert!(hook_matches_stage(&hook, &[], "pre-commit"));
        assert!(!hook_matches_stage(&hook, &[], "pre-push"));
    }

    #[test]
    fn test_hook_matches_stage_default_stages() {
        let hook = Hook {
            id: "x".into(),
            entry: None,
            args: vec![],
            stages: vec![],
            files: None,
            exclude: None,
            pass_filenames: true,
            types: vec![],
            types_or: vec![],
        };
        let defaults = vec!["pre-push".to_string()];
        assert!(hook_matches_stage(&hook, &defaults, "pre-push"));
        assert!(!hook_matches_stage(&hook, &defaults, "pre-commit"));
    }

    #[test]
    fn test_hook_matches_stage_no_stages_means_all() {
        let hook = Hook {
            id: "x".into(),
            entry: None,
            args: vec![],
            stages: vec![],
            files: None,
            exclude: None,
            pass_filenames: true,
            types: vec![],
            types_or: vec![],
        };
        assert!(hook_matches_stage(&hook, &[], "pre-commit"));
        assert!(hook_matches_stage(&hook, &[], "pre-push"));
        assert!(hook_matches_stage(&hook, &[], "commit-msg"));
    }

    // -- types → glob --

    #[test]
    fn test_types_to_glob_single() {
        assert_eq!(types_to_glob(&["python".into()], &[]), Some("*.py".into()));
    }

    #[test]
    fn test_types_to_glob_or() {
        let glob = types_to_glob(&[], &["javascript".into(), "typescript".into()]).unwrap();
        assert!(glob.contains("js"));
        assert!(glob.contains("ts"));
        assert!(glob.starts_with("*.{"));
    }

    #[test]
    fn test_types_to_glob_yaml_multi_ext() {
        let glob = types_to_glob(&["yaml".into()], &[]).unwrap();
        assert!(glob.contains("yml"));
        assert!(glob.contains("yaml"));
    }

    #[test]
    fn test_types_to_glob_skips_generic_types() {
        assert_eq!(types_to_glob(&["file".into()], &[]), None);
        assert_eq!(types_to_glob(&["text".into()], &[]), None);
    }

    #[test]
    fn test_types_to_glob_mixed_types_and_types_or() {
        let glob = types_to_glob(&["python".into()], &["ruby".into()]).unwrap();
        assert!(glob.contains("py"));
        assert!(glob.contains("rb"));
    }

    // -- hook translation --

    #[test]
    fn test_translate_hook_basic() {
        let hook = Hook {
            id: "black".into(),
            entry: Some("black".into()),
            args: vec![],
            stages: vec![],
            files: None,
            exclude: None,
            pass_filenames: true,
            types: vec!["python".into()],
            types_or: vec![],
        };
        let cmd = translate_hook(&hook).unwrap();
        let run = cmd.get("run").unwrap().as_str().unwrap();
        assert_eq!(run, "black {staged_files}");
        assert_eq!(cmd.get("glob").unwrap().as_str().unwrap(), "*.py");
    }

    #[test]
    fn test_translate_hook_with_args() {
        let hook = Hook {
            id: "flake8".into(),
            entry: Some("flake8".into()),
            args: vec!["--max-line-length=100".into()],
            stages: vec![],
            files: None,
            exclude: None,
            pass_filenames: true,
            types: vec![],
            types_or: vec![],
        };
        let cmd = translate_hook(&hook).unwrap();
        let run = cmd.get("run").unwrap().as_str().unwrap();
        assert_eq!(run, "flake8 --max-line-length=100 {staged_files}");
    }

    #[test]
    fn test_translate_hook_no_pass_filenames() {
        let hook = Hook {
            id: "check".into(),
            entry: Some("./check.sh".into()),
            args: vec![],
            stages: vec![],
            files: None,
            exclude: None,
            pass_filenames: false,
            types: vec![],
            types_or: vec![],
        };
        let cmd = translate_hook(&hook).unwrap();
        let run = cmd.get("run").unwrap().as_str().unwrap();
        assert_eq!(run, "./check.sh");
    }

    #[test]
    fn test_translate_hook_with_files_and_exclude() {
        let hook = Hook {
            id: "lint".into(),
            entry: Some("lint".into()),
            args: vec![],
            stages: vec![],
            files: Some(r"\.py$".into()),
            exclude: Some(r"^tests/".into()),
            pass_filenames: true,
            types: vec![],
            types_or: vec![],
        };
        let cmd = translate_hook(&hook).unwrap();
        assert_eq!(cmd.get("files").unwrap().as_str().unwrap(), r"\.py$");
        assert_eq!(cmd.get("exclude").unwrap().as_str().unwrap(), r"^tests/");
    }

    #[test]
    fn test_translate_hook_no_entry_returns_none() {
        let hook = Hook {
            id: "remote-only".into(),
            entry: None,
            args: vec![],
            stages: vec![],
            files: None,
            exclude: None,
            pass_filenames: true,
            types: vec![],
            types_or: vec![],
        };
        assert!(translate_hook(&hook).is_none());
    }

    // -- full adapter integration --

    #[test]
    fn test_generate_config_local_hooks() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
repos:
  - repo: local
    hooks:
      - id: black
        name: black
        entry: black
        language: system
        types: [python]
      - id: isort
        name: isort
        entry: isort
        language: system
        types: [python]
"#,
        );

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("black {staged_files}"), "black cmd: {out}");
        assert!(out.contains("isort {staged_files}"), "isort cmd: {out}");
        assert!(out.contains("*.py"), "glob: {out}");
    }

    #[test]
    fn test_generate_config_skips_remote_repos() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v4.0.0
    hooks:
      - id: trailing-whitespace
"#,
        );

        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_none()
        );
    }

    #[test]
    fn test_generate_config_mixed_local_and_remote() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
repos:
  - repo: https://github.com/psf/black
    rev: "22.10.0"
    hooks:
      - id: black
  - repo: local
    hooks:
      - id: mycheck
        name: mycheck
        entry: ./scripts/check.sh
        language: system
        pass_filenames: false
"#,
        );

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(out.contains("mycheck"), "local hook present: {out}");
        assert!(out.contains("./scripts/check.sh"), "entry mapped: {out}");
        assert!(!out.contains("staged_files"), "no filenames: {out}");
    }

    #[test]
    fn test_generate_config_respects_stages() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
repos:
  - repo: local
    hooks:
      - id: fmt
        entry: fmt
        language: system
        stages: [pre-commit]
      - id: test
        entry: test
        language: system
        stages: [pre-push]
"#,
        );

        let commit_config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&commit_config).unwrap();
        assert!(out.contains("fmt"), "fmt in pre-commit: {out}");
        assert!(!out.contains("test"), "test not in pre-commit: {out}");

        let push_config = adapter().generate_config(dir.path(), "pre-push").unwrap();
        let out = serde_yaml::to_string(&push_config).unwrap();
        assert!(!out.contains("fmt"), "fmt not in pre-push: {out}");
        assert!(out.contains("test"), "test in pre-push: {out}");
    }

    #[test]
    fn test_generate_config_respects_default_stages() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
default_stages: [pre-commit]
repos:
  - repo: local
    hooks:
      - id: fmt
        entry: fmt
        language: system
"#,
        );

        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_some()
        );
        assert!(adapter().generate_config(dir.path(), "pre-push").is_none());
    }

    #[test]
    fn test_generate_config_hook_stages_override_default() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
default_stages: [pre-commit]
repos:
  - repo: local
    hooks:
      - id: fmt
        entry: fmt
        language: system
        stages: [pre-push]
"#,
        );

        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_none()
        );
        assert!(adapter().generate_config(dir.path(), "pre-push").is_some());
    }

    #[test]
    fn test_generate_config_with_args_and_types_or() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"
repos:
  - repo: local
    hooks:
      - id: eslint
        entry: eslint
        args: [--fix]
        language: system
        types_or: [javascript, typescript, jsx, tsx]
"#,
        );

        let config = adapter().generate_config(dir.path(), "pre-commit").unwrap();
        let out = serde_yaml::to_string(&config).unwrap();
        assert!(
            out.contains("eslint --fix {staged_files}"),
            "entry+args: {out}"
        );
        assert!(out.contains("js"), "glob has js: {out}");
        assert!(out.contains("ts"), "glob has ts: {out}");
        assert!(out.contains("jsx"), "glob has jsx: {out}");
        assert!(out.contains("tsx"), "glob has tsx: {out}");
    }

    #[test]
    fn test_generate_config_empty_repos() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "repos: []\n");
        assert!(
            adapter()
                .generate_config(dir.path(), "pre-commit")
                .is_none()
        );
    }
}
