# herdr-lazy

> Be lazy. Install one plugin, get a sensible herd — then customize.

A curated, batteries-included plugin **distro** and **manager** for
[herdr](https://herdr.dev), in the spirit of LazyVim:

- **Manager layer** — a declarative bundle file plus `sync` that converges your
  installed plugins to it. (the lazy.nvim idea)
- **Distro layer** — `init` drops a curated default set so a fresh herdr is
  useful immediately, without hunting the marketplace. (the LazyVim idea)

herdr-lazy is itself a herdr plugin: it drives the herdr CLI (via
`HERDR_BIN_PATH`) to install/list/uninstall the *other* plugins.

## Status: early MVP

Written in Rust (same language as herdr core), std-only, no dependencies.

Commands:

| command | what it does |
|---|---|
| `probe` | verify the plugin↔herdr CLI bridge and dump `plugin list`'s real format |
| `init [--force]` | write the curated default bundle |
| `list` | show the desired plugin set |
| `sync [--prune]` | install everything in the bundle that's missing |
| `add <owner/repo>` | add a plugin to the bundle |
| `remove <owner/repo>` | remove a plugin from the bundle |
| `lock` | record the desired set |

## Build & try

```sh
# 1. install Rust if needed:  https://rustup.rs
cargo build --release          # -> target/release/herdr-lazy

# 2. FIRST run probe on a machine with herdr installed.
#    This is the make-or-break check + it reveals the `plugin list` format.
HERDR_BIN_PATH="$(command -v herdr)" ./target/release/herdr-lazy probe

# 3. once probe looks good:
./target/release/herdr-lazy init      # write curated defaults
./target/release/herdr-lazy sync      # install them
```

Files it uses:

- bundle:  `$HERDR_PLUGIN_CONFIG_DIR/plugins.list`  (falls back to `~/.config/herdr-lazy/`)
- lock:    `$HERDR_PLUGIN_STATE_DIR/plugins.lock`   (falls back to `~/.local/state/herdr-lazy/`)

## Pinning

A bundle entry may pin a commit, tag, or branch:

```
owner/repo                 # tracks the default branch
owner/repo@v1.2.0          # pinned
owner/repo@9f3c1ab         # pinned to a commit — reproducible
```

This maps onto herdr's native `plugin install --ref REF`, so a fully-pinned bundle
reproduces the same plugin set on another machine. `lock` reports how many entries
are still floating.

`sync` writes the lock from herdr's own `source.resolved_commit`, so the lock
records the commit that is actually installed — not merely the ref you asked for.
Verified round-trip: uninstall a plugin, feed the lock back in as the bundle, and
`sync` restores the identical commit.

## Known gaps

1. **`--prune` uninstalls on strong matches only.** A match is *strong* when
   herdr's `source` names the repo (`owner` + `repo`), *weak* when only the
   display name lines up. Locally-linked plugins have no owner/repo at all, so
   they are always reported and kept rather than removed — under-removing is
   recoverable, uninstalling the wrong plugin is not.
2. **`DEFAULT_BUNDLE` is not yet curated.** All six entries are real, installable
   plugins, but the set was assembled by hand rather than chosen as the best
   batteries-included default for a new user.

## Design notes

- Dependency-free by choice: the manager is orchestration (shelling out to the
  herdr CLI and git), which std covers. Keeps builds offline and the
  supply-chain surface at zero. Revisit if we add real TOML/git crates.
- The curated default set lives in `DEFAULT_BUNDLE` in `src/main.rs`.
