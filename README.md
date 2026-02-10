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

### Hook execution

When git triggers a hook, it invokes the symlink in `~/.lhm/hooks/`. `lhm` detects the hook name from `argv[0]` and:

1. **Both configs exist** (`~/.lefthook.yaml` + `$REPO/lefthook.yaml`): generates a temp config with `extends:` referencing both, runs `lefthook run <hook>` with `LEFTHOOK_CONFIG` pointing to it
2. **One config exists**: runs `lefthook run <hook>` with `LEFTHOOK_CONFIG` pointing to that file
3. **Neither exists**: falls back to `$REPO/.git/hooks/<hook>` if present and executable
