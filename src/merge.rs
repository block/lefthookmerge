use crate::hooks::is_hook_name;
use serde_yaml::Value;

/// Merge two lefthook configs. Repo takes precedence over global.
pub fn merge_configs(global: Value, repo: Value) -> Value {
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
            if let Some(name) = job.as_mapping().and_then(|m| m.get("name")).and_then(|v| v.as_str()) {
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
                job.as_mapping().and_then(|m| m.get("name")).and_then(|v| v.as_str())
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
        let global =
            yaml("pre-push:\n  commands:\n    test:\n      run: global-test\n    lint:\n      run: global-lint\n");
        let repo = yaml("pre-push:\n  commands:\n    test:\n      run: repo-test\n");
        let merged = merge_configs(global, repo);
        let out = to_yaml(&merged);
        assert!(out.contains("repo-test"), "repo should win: {out}");
        assert!(!out.contains("global-test"), "global test should be gone: {out}");
        assert!(out.contains("global-lint"), "global-only lint preserved: {out}");
    }

    #[test]
    fn test_merge_configs_cross_format_commands_vs_jobs() {
        let global =
            yaml("pre-push:\n  commands:\n    test:\n      run: global-test\n    lint:\n      run: global-lint\n");
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
        let global = yaml("prepare-commit-msg:\n  commands:\n    aittributor:\n      run: aittributor\n");
        let repo = yaml("pre-commit:\n  jobs:\n    - name: fmt\n      run: just fmt\n");
        let merged = merge_configs(global, repo);
        let out = to_yaml(&merged);
        assert!(out.contains("prepare-commit-msg"), "global-only hook kept: {out}");
        assert!(out.contains("aittributor"), "global command kept: {out}");
        assert!(out.contains("pre-commit"), "repo hook kept: {out}");
    }

    #[test]
    fn test_merge_jobs_named_dedup() {
        let global = yaml("- name: test\n  run: global-test\n- name: unique\n  run: global-unique\n");
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
        assert!(out.contains("prepare-commit-msg"), "global hook kept: {out}");
        assert!(out.contains("aittributor"), "global command kept: {out}");

        // No duplicate commands — global commands with same names stripped
        assert!(!out.contains("grep -qe ^test"), "global test stripped: {out}");
        assert!(!out.contains("grep -qe ^lint"), "global lint stripped: {out}");
        assert!(!out.contains("grep -qe ^fmt"), "global fmt stripped: {out}");

        // Repo jobs present
        assert!(out.contains("just fmt"), "repo fmt: {out}");
        assert!(out.contains("just lint"), "repo lint: {out}");
        assert!(out.contains("just test"), "repo test: {out}");
    }
}
