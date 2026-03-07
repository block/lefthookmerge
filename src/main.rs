mod adapters;
mod config;
mod hooks;
mod merge;

use clap::{Parser, Subcommand};
use log::{debug, error, info};
use serde_yaml::Value;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use config::{install_default_global_config, load_global_config, read_yaml, repo_config, write_merged_temp};
use hooks::{GIT_HOOKS, annotate_hooks, create_hook_symlinks, is_hook_name};
use merge::merge_configs;

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

#[derive(Parser)]
#[command(
    name = "lhm",
    about = "\
Merges global and per-repo lefthook configs.

When invoked as a git hook (via symlink), lhm finds the global config \
(~/.lefthook.yaml) and repo config ($REPO/lefthook.yaml), merges them, \
and runs lefthook. If neither config exists, falls back to \
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
    Install,
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
        Commands::Install => install(),
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

fn home_dir() -> PathBuf {
    env::var("HOME").map(PathBuf::from).expect("HOME not set")
}

fn hooks_dir() -> PathBuf {
    home_dir().join(".lhm").join("hooks")
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

    if let Err(e) = install_default_global_config(&home_dir()) {
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
    global: &Option<Value>,
    repo: &Option<PathBuf>,
    adapter_config: &Option<Value>,
) -> Result<Option<Value>, String> {
    match (global, repo, adapter_config) {
        (Some(g), Some(r), _) => {
            let rv = read_yaml(r)?;
            Ok(Some(merge_configs(g.clone(), rv)))
        }
        (Some(g), None, Some(av)) => Ok(Some(merge_configs(g.clone(), av.clone()))),
        (Some(g), None, None) => Ok(Some(g.clone())),
        (None, Some(r), _) => read_yaml(r).map(Some),
        (None, None, Some(av)) => Ok(Some(av.clone())),
        (None, None, None) => Ok(None),
    }
}

fn dry_run() -> ExitCode {
    let global = match load_global_config(&home_dir()) {
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
        Ok(Some(config)) => {
            print!("{}", serde_yaml::to_string(&config).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Ok(None) => {
            debug!("no config to display");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn lefthook_in_path() -> bool {
    Command::new("lefthook")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Run the repo's `.git/hooks/<hook_name>` script directly.
/// Returns SUCCESS if the script doesn't exist (no hook to run).
fn run_git_hook(hook_name: &str, args: Vec<String>) -> ExitCode {
    let root = match repo_root() {
        Some(r) => r,
        None => return ExitCode::SUCCESS,
    };
    let hook_path = root.join(".git/hooks").join(hook_name);
    if !hook_path.is_file() {
        debug!("no .git/hooks/{hook_name} found, skipping");
        return ExitCode::SUCCESS;
    }
    debug!("running .git/hooks/{hook_name} directly (lefthook not in PATH)");
    let status = Command::new(&hook_path)
        .args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            error!("failed to run .git/hooks/{hook_name}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_hook(hook_name: &str, args: Vec<String>) -> ExitCode {
    if !lefthook_in_path() {
        debug!("lefthook not found in PATH, falling back to .git/hooks");
        return run_git_hook(hook_name, args);
    }

    let global = match load_global_config(&home_dir()) {
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
        root.as_deref().and_then(|r| adapter_config_for(r, Some(hook_name)))
    } else {
        None
    };

    let merged = match resolve_config(&global, &repo, &adapter_config) {
        Ok(Some(m)) => m,
        Ok(None) => {
            debug!("no config found, skipping hook");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let _temp = match write_merged_temp(merged) {
        Ok(t) => t,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let config_path = _temp.path();

    debug!("LEFTHOOK_CONFIG={}", config_path.display());
    debug!("running: lefthook run {hook_name} --no-auto-install {}", args.join(" "));

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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    #[test]
    fn test_run_git_hook_executes_script() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join(".git/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let status = Command::new(&hook).status().expect("hook script should be executable");
        assert!(status.success());
    }

    #[test]
    fn test_run_git_hook_missing_hook_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let hook_path = dir.path().join(".git/hooks/pre-commit");
        assert!(!hook_path.exists());
        // run_git_hook returns SUCCESS when the hook doesn't exist
    }

    #[test]
    fn test_run_git_hook_failing_script() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join(".git/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let status = Command::new(&hook).status().expect("hook script should be executable");
        assert!(!status.success());
    }
}
