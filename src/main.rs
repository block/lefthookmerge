use clap::{Parser, Subcommand};
use serde_yaml::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::NamedTempFile;

static DEBUG: AtomicBool = AtomicBool::new(false);

macro_rules! debug {
    ($($arg:tt)*) => {
        if DEBUG.load(Ordering::Relaxed) {
            eprintln!("lhm: debug: {}", format!($($arg)*));
        }
    };
}

fn init_debug() {
    if env::var("LHM_DEBUG").is_ok_and(|v| v == "1" || v == "true") {
        DEBUG.store(true, Ordering::Relaxed);
    }
}

const GIT_HOOKS: &[&str] = &[
    "applypatch-msg",
    "commit-msg",
    "fsmonitor-watchman",
    "post-update",
    "pre-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "pre-push",
    "pre-rebase",
    "pre-receive",
    "prepare-commit-msg",
    "push-to-checkout",
    "update",
];

#[derive(Parser)]
#[command(
    name = "lhm",
    about = "\
Merges global and per-repo lefthook configs.

When invoked as a git hook (via symlink), lhm finds the global config \
(~/.lefthook.yaml) and repo config ($REPO/lefthook.yaml), merges them using lefthook's \
extends mechanism, and runs lefthook. If neither config exists, falls back to \
$REPO/.git/hooks/<hook>.

Supported config names: lefthook.<ext>, .lefthook.<ext>, .config/lefthook.<ext>
Supported extensions: yml, yaml, json, jsonc, toml"
)]
struct Cli {
    /// Enable debug logging (also via LHM_DEBUG=1)
    #[arg(long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Configure global core.hooksPath to use lhm
    Install,
}

fn main() -> ExitCode {
    init_debug();

    let invoked_as = invoked_name();

    if is_hook_name(&invoked_as) {
        debug!("invoked as hook: {invoked_as}");
        return run_hook(&invoked_as, env::args().skip(1).collect());
    }

    let cli = Cli::parse();
    if cli.debug {
        DEBUG.store(true, Ordering::Relaxed);
    }
    match cli.command {
        Commands::Install => install(),
    }
}

fn invoked_name() -> String {
    env::args()
        .next()
        .as_deref()
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn is_hook_name(name: &str) -> bool {
    GIT_HOOKS.contains(&name)
}

fn home_dir() -> PathBuf {
    env::var("HOME").map(PathBuf::from).expect("HOME not set")
}

fn hooks_dir() -> PathBuf {
    home_dir().join(".lhm").join("hooks")
}

const LEFTHOOK_EXTENSIONS: &[&str] = &["yml", "yaml", "json", "jsonc", "toml"];

/// Search for a lefthook config file in the given directory.
/// Checks `lefthook.<ext>`, `.lefthook.<ext>`, and optionally `.config/lefthook.<ext>`.
fn find_config(dir: &Path, check_dot_config: bool) -> Option<PathBuf> {
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

fn global_config() -> Option<PathBuf> {
    find_config(&home_dir(), false)
}

fn repo_config(root: &Path) -> Option<PathBuf> {
    find_config(root, true)
}

fn repo_root() -> Option<PathBuf> {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

fn install() -> ExitCode {
    let dir = hooks_dir();
    let binary = env::current_exe().expect("cannot determine lhm binary path");
    debug!("hooks dir: {}", dir.display());
    debug!("binary path: {}", binary.display());

    if let Err(e) = create_hook_symlinks(&dir, &binary) {
        eprintln!("lhm: {e}");
        return ExitCode::FAILURE;
    }

    let status = Command::new("git")
        .args(["config", "--global", "core.hooksPath"])
        .arg(&dir)
        .status();

    match status {
        Ok(s) if s.success() => {
            eprintln!("lhm: installed hooks to {}", dir.display());
            eprintln!("lhm: set core.hooksPath = {}", dir.display());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("lhm: failed to set core.hooksPath");
            ExitCode::FAILURE
        }
    }
}

fn create_hook_symlinks(dir: &Path, binary: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;

    for hook in GIT_HOOKS {
        let link = dir.join(hook);
        let _ = fs::remove_file(&link);
        symlink(binary, &link).map_err(|e| format!("failed to symlink {}: {e}", link.display()))?;
    }
    Ok(())
}

fn run_hook(hook_name: &str, args: Vec<String>) -> ExitCode {
    let global = global_config();
    let root = repo_root();
    let repo = root.as_deref().and_then(repo_config);

    debug!("repo root: {:?}", root);
    debug!("global config: {:?}", global);
    debug!("repo config: {:?}", repo);

    if global.is_none() && repo.is_none() {
        debug!("no lefthook configs found, falling back");
        return run_fallback_hook(hook_name, &args);
    }

    let config_result = match (&global, &repo) {
        (Some(g), Some(r)) => build_merged_config(g, r),
        (Some(g), None) => Ok(ConfigSource::Path(g.clone())),
        (None, Some(r)) => Ok(ConfigSource::Path(r.clone())),
        (None, None) => unreachable!(),
    };

    let (config_path, _temp) = match config_result {
        Ok(ConfigSource::Path(p)) => (p, None),
        Ok(ConfigSource::Temp(t)) => {
            let path = t.path().to_path_buf();
            (path, Some(t))
        }
        Err(e) => {
            eprintln!("lhm: {e}");
            return ExitCode::FAILURE;
        }
    };

    debug!("LEFTHOOK_CONFIG={}", config_path.display());
    debug!(
        "running: lefthook run {hook_name} --no-auto-install {}",
        args.join(" ")
    );

    let status = Command::new("lefthook")
        .arg("run")
        .arg(hook_name)
        .arg("--no-auto-install")
        .args(&args)
        .env("LEFTHOOK_CONFIG", &config_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("lhm: failed to run lefthook: {e}");
            ExitCode::FAILURE
        }
    }
}

enum ConfigSource {
    Path(PathBuf),
    Temp(NamedTempFile),
}

fn build_merged_config(global: &Path, repo: &Path) -> Result<ConfigSource, String> {
    let global_yaml = read_yaml(global)?;
    let repo_yaml = read_yaml(repo)?;
    let merged = merge_configs(global_yaml, repo_yaml);

    let content =
        serde_yaml::to_string(&merged).map_err(|e| format!("failed to serialize config: {e}"))?;
    debug!("merged config:\n{content}");

    let mut tmp = tempfile::Builder::new()
        .suffix(".yml")
        .tempfile()
        .map_err(|e| format!("failed to create temp file: {e}"))?;
    write!(tmp, "{content}").map_err(|e| format!("failed to write temp config: {e}"))?;
    Ok(ConfigSource::Temp(tmp))
}

fn read_yaml(path: &Path) -> Result<Value, String> {
    let content =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

/// Merge two lefthook configs. Repo takes precedence over global.
fn merge_configs(global: Value, repo: Value) -> Value {
    match (global, repo) {
        (Value::Mapping(mut global), Value::Mapping(repo)) => {
            for (key, repo_val) in repo {
                let key_str = key.as_str().unwrap_or("");
                if is_hook_name(key_str) {
                    if let Some(global_val) = global.remove(&key) {
                        global.insert(key, merge_hook(global_val, repo_val));
                    } else {
                        global.insert(key, repo_val);
                    }
                } else {
                    global.insert(key, repo_val);
                }
            }
            Value::Mapping(global)
        }
        (_, repo) => repo,
    }
}

/// Merge two hook definitions. For commands/scripts maps, merge by name.
/// For jobs lists, merge named jobs by name and append unnamed ones.
/// When formats differ (commands vs jobs), repo names suppress matching global names.
/// For all other keys, repo wins.
fn merge_hook(global: Value, repo: Value) -> Value {
    match (global, repo) {
        (Value::Mapping(mut global), Value::Mapping(repo)) => {
            // Collect repo task names across all formats for cross-format dedup
            let repo_task_names = collect_task_names_from_mapping(&repo);

            // Remove global tasks that are overridden by repo (cross-format)
            if !repo_task_names.is_empty() {
                strip_names_from_commands(&mut global, &repo_task_names);
                strip_names_from_scripts(&mut global, &repo_task_names);
                strip_names_from_jobs(&mut global, &repo_task_names);
            }

            for (key, repo_val) in repo {
                let key_str = key.as_str().unwrap_or("");
                match key_str {
                    "commands" | "scripts" => {
                        if let Some(global_val) = global.remove(&key) {
                            global.insert(key, merge_maps(global_val, repo_val));
                        } else {
                            global.insert(key, repo_val);
                        }
                    }
                    "jobs" => {
                        if let Some(global_val) = global.remove(&key) {
                            global.insert(key, merge_jobs(global_val, repo_val));
                        } else {
                            global.insert(key, repo_val);
                        }
                    }
                    _ => {
                        global.insert(key, repo_val);
                    }
                }
            }

            Value::Mapping(global)
        }
        (_, repo) => repo,
    }
}

fn collect_task_names_from_mapping(mapping: &serde_yaml::Mapping) -> Vec<String> {
    let mut names = Vec::new();

    // Names from commands/scripts (map keys)
    for section in ["commands", "scripts"] {
        if let Some(Value::Mapping(m)) = mapping.get(Value::String(section.to_string())) {
            for key in m.keys() {
                if let Some(s) = key.as_str() {
                    names.push(s.to_string());
                }
            }
        }
    }

    // Names from jobs (name field)
    if let Some(Value::Sequence(jobs)) = mapping.get(Value::String("jobs".to_string())) {
        for job in jobs {
            if let Some(name) = job
                .as_mapping()
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str())
            {
                names.push(name.to_string());
            }
        }
    }

    names
}

fn strip_names_from_commands(mapping: &mut serde_yaml::Mapping, names: &[String]) {
    let key = Value::String("commands".to_string());
    if let Some(Value::Mapping(cmds)) = mapping.get_mut(&key) {
        cmds.retain(|k, _| k.as_str().is_none_or(|s| !names.contains(&s.to_string())));
        if cmds.is_empty() {
            mapping.remove(&key);
        }
    }
}

fn strip_names_from_scripts(mapping: &mut serde_yaml::Mapping, names: &[String]) {
    let key = Value::String("scripts".to_string());
    if let Some(Value::Mapping(scripts)) = mapping.get_mut(&key) {
        scripts.retain(|k, _| k.as_str().is_none_or(|s| !names.contains(&s.to_string())));
        if scripts.is_empty() {
            mapping.remove(&key);
        }
    }
}

fn strip_names_from_jobs(mapping: &mut serde_yaml::Mapping, names: &[String]) {
    let key = Value::String("jobs".to_string());
    if let Some(Value::Sequence(jobs)) = mapping.get_mut(&key) {
        jobs.retain(|job| {
            job.as_mapping()
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str())
                .is_none_or(|name| !names.contains(&name.to_string()))
        });
        if jobs.is_empty() {
            mapping.remove(&key);
        }
    }
}

/// Merge two YAML maps by key. Repo values override global values.
fn merge_maps(global: Value, repo: Value) -> Value {
    match (global, repo) {
        (Value::Mapping(mut global), Value::Mapping(repo)) => {
            for (key, repo_val) in repo {
                global.insert(key, repo_val);
            }
            Value::Mapping(global)
        }
        (_, repo) => repo,
    }
}

/// Merge two jobs lists. Named jobs (with `name` field) are merged by name
/// with repo taking precedence. Unnamed jobs are appended (global first, then repo).
fn merge_jobs(global: Value, repo: Value) -> Value {
    match (&global, &repo) {
        (Value::Sequence(global_jobs), Value::Sequence(repo_jobs)) => {
            fn job_name(job: &Value) -> Option<&str> {
                job.as_mapping()
                    .and_then(|m| m.get("name"))
                    .and_then(|v| v.as_str())
            }

            let repo_names: Vec<Option<&str>> = repo_jobs.iter().map(|j| job_name(j)).collect();

            let mut result: Vec<Value> = Vec::new();

            // Add global jobs, skipping named ones that repo overrides
            for job in global_jobs {
                if let Some(name) = job_name(job) {
                    if repo_names.iter().any(|rn| *rn == Some(name)) {
                        continue;
                    }
                }
                result.push(job.clone());
            }

            // Add all repo jobs
            result.extend(repo_jobs.iter().cloned());

            Value::Sequence(result)
        }
        _ => repo,
    }
}

fn run_fallback_hook(hook_name: &str, args: &[String]) -> ExitCode {
    let Some(root) = repo_root() else {
        debug!("not in a git repo, skipping fallback");
        return ExitCode::SUCCESS;
    };

    let hook_path = root.join(".git").join("hooks").join(hook_name);
    if !hook_path.is_file() {
        debug!("no fallback hook at {}", hook_path.display());
        return ExitCode::SUCCESS;
    }

    let Ok(meta) = hook_path.metadata() else {
        return ExitCode::SUCCESS;
    };
    if meta.permissions().mode() & 0o111 == 0 {
        debug!("fallback hook not executable: {}", hook_path.display());
        return ExitCode::SUCCESS;
    }

    debug!("running fallback hook: {}", hook_path.display());

    let status = Command::new(&hook_path)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("lhm: failed to run {}: {e}", hook_path.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_hook_name() {
        assert!(is_hook_name("pre-commit"));
        assert!(is_hook_name("commit-msg"));
        assert!(is_hook_name("pre-push"));
        assert!(is_hook_name("update"));
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
    fn test_find_config_yaml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lefthook.yaml"), "").unwrap();
        assert_eq!(
            find_config(dir.path(), false),
            Some(dir.path().join("lefthook.yaml"))
        );
    }

    #[test]
    fn test_find_config_yml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lefthook.yml"), "").unwrap();
        assert_eq!(
            find_config(dir.path(), false),
            Some(dir.path().join("lefthook.yml"))
        );
    }

    #[test]
    fn test_find_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("lefthook.toml"), "").unwrap();
        assert_eq!(
            find_config(dir.path(), false),
            Some(dir.path().join("lefthook.toml"))
        );
    }

    #[test]
    fn test_find_config_dotted() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".lefthook.json"), "").unwrap();
        assert_eq!(
            find_config(dir.path(), false),
            Some(dir.path().join(".lefthook.json"))
        );
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
        assert_eq!(
            find_config(dir.path(), false),
            Some(dir.path().join("lefthook.yml"))
        );
    }

    fn yaml(s: &str) -> Value {
        serde_yaml::from_str(s).unwrap()
    }

    fn to_yaml(v: &Value) -> String {
        serde_yaml::to_string(v).unwrap()
    }

    #[test]
    fn test_merge_configs_repo_overrides_scalars() {
        let global = yaml("output:\n  - success\nmin_version: '1.0'\n");
        let repo = yaml("output:\n  - failure\nskip_lfs: true\n");
        let merged = merge_configs(global, repo);
        let out = to_yaml(&merged);
        assert!(out.contains("skip_lfs: true"));
        assert!(out.contains("failure"));
        assert!(out.contains("min_version"));
    }

    #[test]
    fn test_merge_configs_commands_dedup() {
        let global = yaml(
            "pre-push:\n  commands:\n    test:\n      run: global-test\n    lint:\n      run: global-lint\n",
        );
        let repo = yaml("pre-push:\n  commands:\n    test:\n      run: repo-test\n");
        let merged = merge_configs(global, repo);
        let out = to_yaml(&merged);
        assert!(out.contains("repo-test"), "repo should win: {out}");
        assert!(
            !out.contains("global-test"),
            "global test should be gone: {out}"
        );
        assert!(
            out.contains("global-lint"),
            "global-only lint preserved: {out}"
        );
    }

    #[test]
    fn test_merge_configs_cross_format_commands_vs_jobs() {
        let global = yaml(
            "pre-push:\n  commands:\n    test:\n      run: global-test\n    lint:\n      run: global-lint\n",
        );
        let repo = yaml(
            "pre-push:\n  jobs:\n    - name: test\n      run: repo-test\n    - name: lint\n      run: repo-lint\n",
        );
        let merged = merge_configs(global, repo);
        let out = to_yaml(&merged);
        // Global commands with same names should be stripped
        assert!(!out.contains("global-test"), "global test stripped: {out}");
        assert!(!out.contains("global-lint"), "global lint stripped: {out}");
        // Repo jobs should be present
        assert!(out.contains("repo-test"), "repo test present: {out}");
        assert!(out.contains("repo-lint"), "repo lint present: {out}");
    }

    #[test]
    fn test_merge_configs_global_only_hook_preserved() {
        let global =
            yaml("prepare-commit-msg:\n  commands:\n    aittributor:\n      run: aittributor\n");
        let repo = yaml("pre-commit:\n  jobs:\n    - name: fmt\n      run: just fmt\n");
        let merged = merge_configs(global, repo);
        let out = to_yaml(&merged);
        assert!(
            out.contains("prepare-commit-msg"),
            "global-only hook kept: {out}"
        );
        assert!(out.contains("aittributor"), "global command kept: {out}");
        assert!(out.contains("pre-commit"), "repo hook kept: {out}");
    }

    #[test]
    fn test_merge_jobs_named_dedup() {
        let global =
            yaml("- name: test\n  run: global-test\n- name: unique\n  run: global-unique\n");
        let repo = yaml("- name: test\n  run: repo-test\n");
        let merged = merge_jobs(global, repo);
        let out = to_yaml(&merged);
        assert!(out.contains("repo-test"), "repo wins: {out}");
        assert!(!out.contains("global-test"), "global test removed: {out}");
        assert!(out.contains("global-unique"), "global-only job kept: {out}");
    }

    #[test]
    fn test_merge_jobs_unnamed_appended() {
        let global = yaml("- run: global-unnamed\n");
        let repo = yaml("- run: repo-unnamed\n");
        let merged = merge_jobs(global, repo);
        let out = to_yaml(&merged);
        assert!(out.contains("global-unnamed"), "global unnamed kept: {out}");
        assert!(out.contains("repo-unnamed"), "repo unnamed kept: {out}");
    }

    #[test]
    fn test_merge_real_configs() {
        let global = yaml(
            r#"
output:
  - success
  - failure
pre-push:
  parallel: true
  commands:
    test:
      run: grep -qe ^test Justfile 2> /dev/null && just test
    lint:
      run: grep -qe ^lint Justfile 2> /dev/null && just lint
prepare-commit-msg:
  commands:
    aittributor:
      run: aittributor {1}
pre-commit:
  commands:
    fmt:
      run: grep -qe ^fmt Justfile 2> /dev/null && just fmt
"#,
        );
        let repo = yaml(
            r#"
skip_lfs: true
output:
  - success
  - failure
pre-commit:
  parallel: true
  jobs:
    - name: fmt
      run: just fmt
pre-push:
  parallel: true
  jobs:
    - name: lint
      run: just lint
    - name: test
      run: just test
"#,
        );
        let merged = merge_configs(global, repo);
        let out = to_yaml(&merged);

        // Repo scalars win
        assert!(out.contains("skip_lfs: true"), "repo skip_lfs: {out}");

        // Global-only hook preserved
        assert!(
            out.contains("prepare-commit-msg"),
            "global hook kept: {out}"
        );
        assert!(out.contains("aittributor"), "global command kept: {out}");

        // No duplicate commands — global commands with same names stripped
        assert!(
            !out.contains("grep -qe ^test"),
            "global test stripped: {out}"
        );
        assert!(
            !out.contains("grep -qe ^lint"),
            "global lint stripped: {out}"
        );
        assert!(!out.contains("grep -qe ^fmt"), "global fmt stripped: {out}");

        // Repo jobs present
        assert!(out.contains("just fmt"), "repo fmt: {out}");
        assert!(out.contains("just lint"), "repo lint: {out}");
        assert!(out.contains("just test"), "repo test: {out}");
    }
}
