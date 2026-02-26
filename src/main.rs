mod adapters;

use clap::{Parser, Subcommand};
use log::{debug, error, info};
use serde_yaml::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use tempfile::NamedTempFile;

fn init_logger(cli_debug: bool) {
    let debug_enabled = cli_debug || env::var("LHM_DEBUG").is_ok_and(|v| v == "1" || v == "true");

    let level = if debug_enabled {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    env_logger::Builder::new()
        .filter_level(level)
        .format(|buf, record| {
            use std::io::Write;
            match record.level() {
                log::Level::Debug => writeln!(buf, "lhm: debug: {}", record.args()),
                log::Level::Info => writeln!(buf, "lhm: {}", record.args()),
                _ => writeln!(
                    buf,
                    "lhm: {}: {}",
                    record.level().as_str().to_lowercase(),
                    record.args()
                ),
            }
        })
        .init();
}

const GIT_HOOKS: &[&str] = &[
    "applypatch-msg",
    "commit-msg",
    "fsmonitor-watchman",
    "post-applypatch",
    "post-checkout",
    "post-commit",
    "post-merge",
    "post-receive",
    "post-rewrite",
    "post-update",
    "pre-applypatch",
    "pre-auto-gc",
    "pre-commit",
    "pre-merge-commit",
    "pre-push",
    "pre-rebase",
    "pre-receive",
    "prepare-commit-msg",
    "push-to-checkout",
    "reference-transaction",
    "sendemail-validate",
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
the adapter system.

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
    Install {
        /// Write the default global config to ~/.lefthook.yaml
        #[arg(long)]
        default_config: bool,
    },
    /// Print the merged config that would be used, then exit
    DryRun,
}

fn main() -> ExitCode {
    let invoked_as = invoked_name();

    if is_hook_name(&invoked_as) {
        init_logger(false);
        debug!("invoked as hook: {invoked_as}");
        return run_hook(&invoked_as, env::args().skip(1).collect());
    }

    let cli = Cli::parse();
    init_logger(cli.debug);
    match cli.command {
        Commands::Install { default_config } => install(default_config),
        Commands::DryRun => dry_run(),
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

const DEFAULT_GLOBAL_CONFIG: &str = r#"# Global lefthook configuration
output:
  - success
  - failure
pre-push:
  parallel: true
  commands:
    test:
      run: just test
      skip:
        - run: lefthook --dry-run test
    lint:
      run: just lint
      skip:
        - run: lefthook --dry-run lint
prepare-commit-msg:
  commands:
    aittributor:
      run: aittributor {1}
      skip:
        - run: which aittributor > /dev/null
pre-commit:
  commands:
    fmt:
      stage_fixed: true
      run: just fmt
      skip:
        - run: lefthook --dry-run fmt
"#;

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

/// Write the default global config to `~/.lefthook.yaml` if no global config exists.
fn install_default_global_config(home: &Path) -> Result<(), String> {
    if find_config(home, false).is_some() {
        debug!("global config already exists, skipping default");
        return Ok(());
    }
    let path = home.join(".lefthook.yaml");
    fs::write(&path, DEFAULT_GLOBAL_CONFIG)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    info!("created default global config at {}", path.display());
    Ok(())
}

/// Parse `DEFAULT_GLOBAL_CONFIG` as YAML.
fn parse_default_global_config() -> Value {
    serde_yaml::from_str(DEFAULT_GLOBAL_CONFIG).expect("default config is valid YAML")
}

/// Load the effective global config: from `~/.lefthook.yaml` if it exists,
/// otherwise fall back to the built-in `DEFAULT_GLOBAL_CONFIG`.
fn load_global_config() -> Result<Value, String> {
    match global_config() {
        Some(path) => read_yaml(&path),
        None => {
            debug!("no global config file found, using built-in default");
            Ok(parse_default_global_config())
        }
    }
}

fn install(default_config: bool) -> ExitCode {
    let dir = hooks_dir();
    let binary = env::current_exe().expect("cannot determine lhm binary path");
    debug!("hooks dir: {}", dir.display());
    debug!("binary path: {}", binary.display());

    if default_config && let Err(e) = install_default_global_config(&home_dir()) {
        error!("{e}");
        return ExitCode::FAILURE;
    }

    if let Err(e) = create_hook_symlinks(&dir, &binary) {
        error!("{e}");
        return ExitCode::FAILURE;
    }

    let status = Command::new("git")
        .args(["config", "--global", "core.hooksPath"])
        .arg(&dir)
        .status();

    match status {
        Ok(s) if s.success() => {
            info!("installed hooks to {}", dir.display());
            info!("set core.hooksPath = {}", dir.display());
            ExitCode::SUCCESS
        }
        _ => {
            error!("failed to set core.hooksPath");
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

/// Annotate adapter-generated config with lefthook settings:
/// - `parallel: true` on hooks that don't mutate shared state
/// - `stage_fixed: true` on each command within `pre-commit` and `pre-merge-commit` hooks
fn annotate_hooks(config: Value) -> Value {
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

fn adapter_config_for(root: &Path, hook_name: Option<&str>) -> Option<Value> {
    let adapter = adapters::detect_adapter(root)?;
    debug!("detected adapter: {}", adapter.name());

    if let Some(name) = hook_name {
        let config = adapter.generate_config(root, name);
        if config.is_none() {
            debug!("adapter {} has no config for {name}", adapter.name());
        }
        return config.map(annotate_hooks);
    }

    let mut combined: Option<Value> = None;
    for name in GIT_HOOKS {
        if let Some(config) = adapter.generate_config(root, name) {
            combined = Some(match combined {
                Some(existing) => merge_configs(existing, config),
                None => config,
            });
        }
    }
    combined.map(annotate_hooks)
}

/// Resolve global, repo, and adapter sources into a single merged config.
fn resolve_config(
    global: &Value,
    repo: &Option<PathBuf>,
    adapter_config: &Option<Value>,
) -> Result<Value, String> {
    match (repo, adapter_config) {
        (Some(r), _) => {
            let rv = read_yaml(r)?;
            Ok(merge_configs(global.clone(), rv))
        }
        (None, Some(av)) => Ok(merge_configs(global.clone(), av.clone())),
        (None, None) => Ok(global.clone()),
    }
}

fn dry_run() -> ExitCode {
    let global = match load_global_config() {
        Ok(v) => v,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let root = repo_root();
    let repo = root.as_deref().and_then(repo_config);

    let adapter_config = if repo.is_none() {
        root.as_deref().and_then(|r| adapter_config_for(r, None))
    } else {
        None
    };

    if let Some(ref p) = repo {
        debug!("repo config: {}", p.display());
    }

    match resolve_config(&global, &repo, &adapter_config) {
        Ok(config) => {
            print!("{}", serde_yaml::to_string(&config).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run_hook(hook_name: &str, args: Vec<String>) -> ExitCode {
    let global = match load_global_config() {
        Ok(v) => v,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let root = repo_root();
    let repo = root.as_deref().and_then(repo_config);

    debug!("repo root: {:?}", root);
    debug!("repo config: {:?}", repo);

    let adapter_config = if repo.is_none() {
        root.as_deref()
            .and_then(|r| adapter_config_for(r, Some(hook_name)))
    } else {
        None
    };

    let _temp = match resolve_config(&global, &repo, &adapter_config) {
        Ok(merged) => match write_merged_temp(merged) {
            Ok(t) => t,
            Err(e) => {
                error!("{e}");
                return ExitCode::FAILURE;
            }
        },
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let config_path = _temp.path();

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
        .env("LEFTHOOK_CONFIG", config_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            error!("failed to run lefthook: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Serialize a merged config value to a temp file for lefthook.
fn write_merged_temp(merged: Value) -> Result<NamedTempFile, String> {
    let content =
        serde_yaml::to_string(&merged).map_err(|e| format!("failed to serialize config: {e}"))?;
    debug!("merged config:\n{content}");

    let mut tmp = tempfile::Builder::new()
        .suffix(".yml")
        .tempfile()
        .map_err(|e| format!("failed to create temp file: {e}"))?;
    write!(tmp, "{content}").map_err(|e| format!("failed to write temp config: {e}"))?;
    Ok(tmp)
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
                if let Some(name) = job_name(job)
                    && repo_names.contains(&Some(name))
                {
                    continue;
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

    #[test]
    fn test_install_default_global_config_creates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        install_default_global_config(dir.path()).unwrap();

        let created = dir.path().join(".lefthook.yaml");
        assert!(created.is_file());
        let content = fs::read_to_string(&created).unwrap();
        assert!(content.contains("pre-push:"));
        assert!(content.contains("pre-commit:"));
        assert!(content.contains("prepare-commit-msg:"));
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
    fn test_parse_default_global_config() {
        let val = parse_default_global_config();
        let out = to_yaml(&val);
        assert!(out.contains("pre-push:"));
        assert!(out.contains("pre-commit:"));
        assert!(out.contains("prepare-commit-msg:"));
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
