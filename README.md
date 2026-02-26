# lhm - Merges global and repo lefthook configs

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/block/lefthookmerge/main/install.sh | sh
```

## Overview

This tool is designed to merge global lefthook config with per repo config. `lhm install` configures global
`core.hooksPath` to call `lhm` which dynamically merges the global and repo lefthook configs, if they exist,
using lefthooks' `extends` [mechanism](https://lefthook.dev/configuration/extends.html).

All standard lefthook config file names are supported: `lefthook.<ext>`, `.lefthook.<ext>` (and `.config/lefthook.<ext>`
for repo configs), where `<ext>` is `yml`, `yaml`, `json`, `jsonc`, or `toml`.

## How it works

### `lhm install`

- Creates symlinks for all standard git hooks in `~/.lhm/hooks/`, each pointing to the `lhm` binary
- Sets `git config --global core.hooksPath ~/.lhm/hooks`
- With `--default-config`: writes a default `~/.lefthook.yaml` if no global config exists

### `lhm dry-run`

Prints the merged config that would be used for the current repo, then exits. Useful for verifying what hooks will run.

```sh
lhm dry-run
```

### Hook execution

When git triggers a hook, it invokes the symlink in `~/.lhm/hooks/`. `lhm` detects the hook name from `argv[0]` and:

1. **Global config** is always available: loaded from `~/.lefthook.yaml` if it exists, otherwise a built-in default is used in memory
2. **Both configs exist** (`~/.lefthook.yaml` + `$REPO/lefthook.yaml`): merges global and repo configs, runs `lefthook run <hook>` with `LEFTHOOK_CONFIG` pointing to the merged temp file
3. **Global only** (no repo config or adapter): runs `lefthook run <hook>` with the global config
4. **No repo config, but adapter detected**: generates a dynamic lefthook config from the adapter, merges it with the global config, and runs `lefthook run <hook>`

### Adapters

When a repo has no `lefthook.yaml`, lhm checks for other git hook managers and transparently adapts them. The generated adapter config is merged with `~/.lefthook.yaml` using the standard merging system, so global hooks still apply.

Adapters are tried in this order (first match wins):

| Adapter | Detects | Behavior |
|---------|---------|----------|
| **pre-commit** | `.pre-commit-config.yaml` | Translates `repo: local` hooks into lefthook commands (`entry` + `args` → `run`, `types`/`types_or` → `glob`, `files`/`exclude` preserved). Remote repos are skipped. |
| **husky** | `.husky/` directory | Runs `.husky/<hook>` (if script exists) |
| **hooks-dir** | `.hooks/` or `git-hooks/` directory | Runs `<dir>/<hook>` (if script exists) and all `<dir>/<hook>-*` prefixed scripts as parallel lefthook commands. Checked in order (first match wins). `.git/hooks/` is intentionally excluded to avoid double-executing hooks already handled by dedicated adapters or lhm itself. |

### Debugging

Enable debug logging with `--debug` or `LHM_DEBUG=1`:

```sh
lhm --debug install
LHM_DEBUG=1 git commit
```
