use clap::{Parser, Subcommand};
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use tempfile::NamedTempFile;

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
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Configure global core.hooksPath to use lhm
    Install,
}

fn main() -> ExitCode {
    let invoked_as = invoked_name();

    if is_hook_name(&invoked_as) {
        return run_hook(&invoked_as, env::args().skip(1).collect());
    }

    let cli = Cli::parse();
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

    if global.is_none() && repo.is_none() {
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

    let status = Command::new("lefthook")
        .arg("run")
        .arg(hook_name)
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
    let mut tmp = NamedTempFile::new().map_err(|e| format!("failed to create temp file: {e}"))?;
    write!(
        tmp,
        "extends:\n  - {}\n  - {}\n",
        global.display(),
        repo.display()
    )
    .map_err(|e| format!("failed to write temp config: {e}"))?;
    Ok(ConfigSource::Temp(tmp))
}

fn run_fallback_hook(hook_name: &str, args: &[String]) -> ExitCode {
    let Some(root) = repo_root() else {
        return ExitCode::SUCCESS;
    };

    let hook_path = root.join(".git").join("hooks").join(hook_name);
    if !hook_path.is_file() {
        return ExitCode::SUCCESS;
    }

    let Ok(meta) = hook_path.metadata() else {
        return ExitCode::SUCCESS;
    };
    if meta.permissions().mode() & 0o111 == 0 {
        return ExitCode::SUCCESS;
    }

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
    use std::io::{Read, Seek};

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

    #[test]
    fn test_build_merged_config() {
        let global = Path::new("/home/user/.lefthook.yaml");
        let repo = Path::new("/home/user/project/lefthook.yaml");

        let result = build_merged_config(global, repo).unwrap();
        match result {
            ConfigSource::Temp(mut t) => {
                let mut content = String::new();
                t.as_file_mut().seek(std::io::SeekFrom::Start(0)).unwrap();
                t.as_file_mut().read_to_string(&mut content).unwrap();
                assert_eq!(
                    content,
                    "extends:\n  - /home/user/.lefthook.yaml\n  - /home/user/project/lefthook.yaml\n"
                );
            }
            ConfigSource::Path(_) => panic!("expected Temp variant"),
        }
    }
}
