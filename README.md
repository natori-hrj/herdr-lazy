# herdr-lazy

> Be lazy. Declare the plugins you want; let the tool converge your machine to them.

A declarative plugin **manager** and curated **distro** for [herdr](https://herdr.dev).

herdr installs plugins one imperative command at a time. There is no way to declare the
set you want, and no lockfile — so a working setup cannot be reproduced on another
machine. herdr-lazy adds both.

```
 herdr-lazy  2 ok · 2 to sync · 1 unlisted
 ────────────────────────────────────────────────────────────────
 ✔ cloudmanic/herdr-plus            f32b0825f125
 ✔ smarzban/herdr-file-viewer       10e930332635
 ✗ owner/not-yet-installed          -             in your list, not installed — press s to install
 ↻ owner/pinned@9f3c1ab             b872365a12f4  installed at b872365a12f4, pinned elsewhere — press s …
 + someone/unlisted                 8facfbb2a7bd  installed, not in your list — press x to remove
 ⚑ herdr-lazy                       -             installed as a local link — never removed by prune
 ────────────────────────────────────────────────────────────────
 s sync  u update  x remove extras  r refresh  q quit
```

## What it gives you

- **A declarative plugin list.** One `owner/repo` per line. `sync` converges your
  machine to it — installing what is missing, and (with `--prune`) removing the rest.
- **A real lockfile.** Entries pin to a commit, and the lock records the commit herdr
  actually checked out. Copy the lock to another machine, `sync`, and you get the same
  plugins at the same commits.
- **A manage pane.** A herdr overlay pane with the same operations on single keys.
- **A curated default set.** `init` writes a starting bundle so a fresh herdr is useful
  immediately.

herdr-lazy is itself a herdr plugin: it drives the herdr CLI (via `HERDR_BIN_PATH`) to
manage the *other* plugins.

## Install

Requires herdr ≥ 0.7.0.

```sh
herdr plugin install natori-hrj/herdr-lazy
```

Install fetches a prebuilt binary and verifies its SHA-256; if none matches your platform,
or anything about the download is not exactly right, it falls back to building from source
with [Rust](https://rustup.rs) (≥ 1.78). No toolchain is needed on the fast path.

Then open the manage pane from the command palette (**Lazy: open manage pane**), or
bind it:

```toml
# ~/.config/herdr/config.toml
[[keys.command]]
key = "prefix+shift+l"
type = "plugin_action"
command = "herdr-lazy.manage"
description = "manage plugins"
```

Pick a key that is actually free — `prefix+l` is `focus_pane_right`, and `h`/`j`/`k`/`n`/`p`/
`c`/`g` are taken too. `prefix+?` lists your active bindings.

## Use

```sh
herdr-lazy init          # write the curated default bundle
herdr-lazy list          # show what the bundle asks for
herdr-lazy sync          # install what is missing
herdr-lazy update        # move unpinned entries to their latest commit
herdr-lazy sync --prune  # also remove anything not in the bundle
```

| command | what it does |
|---|---|
| `init [--force]` | write the curated default bundle |
| `list` | show the desired plugin set |
| `sync [--prune]` | converge installed plugins to the bundle |
| `update [<repo>…]` | re-resolve unpinned entries to their latest commit |
| `ui` / `manage` | open the manage pane |
| `add <owner/repo>` | add an entry to the bundle |
| `remove <owner/repo>` | remove an entry from the bundle |
| `lock` | write the lockfile from the current bundle |
| `probe` | dump what the herdr CLI exposes (for debugging) |

Both files live in the directory herdr assigns the plugin — `herdr plugin config-dir
herdr-lazy` prints it:

- `plugins.list` — the set you declare, edited by hand or via `add`/`remove`
- `plugins.lock` — the commits actually installed, rewritten on every `sync`

Run from a shell, herdr-lazy asks herdr for that path rather than guessing, so the CLI and
the manage pane always read the same files.

## Pinning and reproducibility

```
owner/repo             # tracks the default branch
owner/repo@v1.2.0      # pinned to a tag
owner/repo@9f3c1ab     # pinned to a commit — reproducible and checkable
```

These map onto herdr's native `plugin install --ref`. `sync` writes the lock from
herdr's own `source.resolved_commit`, so the lock records what is *installed*, not
merely what was requested.

`sync` also **enforces** commit pins: a plugin sitting at the wrong commit is restored,
not silently accepted. Tag and branch pins cannot be checked locally — herdr resolves
them at install time and does not report the original ref back — so those are flagged
as unverifiable rather than reinstalled on every run.

`update` deliberately skips pinned entries. A pin means "this commit"; moving it
silently would make the lock disagree with the bundle. Edit the bundle to move a pin.

## Safety

`--prune` uninstalls only what it can prove is extraneous. A match is **strong** when
herdr's `source` names the repo (`owner` + `repo`), and **weak** when only the display
name lines up — herdr's `plugin_id` and `name` bear no reliable relation to the repo
(`cloudmanic/herdr-plus` registers as `cloudmanic.herdr-plus`). Prune acts on strong matches only.
Locally-linked plugins have no owner/repo at all and are always kept: herdr-lazy is
normally one, so this also stops prune from removing the tool running it.

Under-removing is recoverable. Uninstalling the wrong plugin is not.

## The default bundle

A distro is an opinion, so here is the reasoning rather than just the list. Two criteria,
in order: prefer what the ecosystem has already vetted, then fill the gaps nothing else
covers. Overlapping plugins are excluded rather than stacked — two plugins that both open
a file pane is a worse default than one.

| plugin | why |
|---|---|
| [cloudmanic/herdr-plus](https://github.com/cloudmanic/herdr-plus) | projects and quick actions; the broadest general-purpose add-on |
| [smarzban/herdr-file-viewer](https://github.com/smarzban/herdr-file-viewer) | git-aware read-only file pane |
| [razajamil/herdr-plugin-workspace-manager](https://github.com/razajamil/herdr-plugin-workspace-manager) | per-workspace tab/pane layouts, applied automatically |
| [natori-hrj/herdr-triage](https://github.com/natori-hrj/herdr-triage) | ranks agents by who needs you most |
| [natori-hrj/herdr-green](https://github.com/natori-hrj/herdr-green) | runs a project's tests when its agent finishes |
| [natori-hrj/herdr-standup](https://github.com/natori-hrj/herdr-standup) | digest of what every agent actually changed |

The last three are by this project's author. They are here because running several agents
at once creates a problem the ecosystem does not otherwise address — knowing which one to
look at, whether its work is sound, and what it did — not because of who wrote them. If
that is not your problem, remove them.

A third criterion showed up during testing: it has to actually install. herdr runs plugin
builds with a minimal PATH that excludes `~/.cargo/bin`, so a plugin whose build is a bare
`cargo build --release` fails on machines where Rust is installed and works fine in your own
shell. herdr-lazy itself works around this (see `scripts/fetch-or-build.sh`), but a default
set cannot hand a new user a failed install.

Deliberately **not** included, despite being good:
[herdr-spreader](https://github.com/yuk1ty/herdr-spreader) (41★) is the better-known layout
plugin, but it hits exactly that build problem, and workspace-manager does the same job with
no build step;
[herdr-reviewr](https://github.com/persiyanov/herdr-reviewr) bundles its own file viewer and
so duplicates herdr-file-viewer (swap, do not add);
[herdr-remote](https://github.com/dcolinmorgan/herdr-remote) and
[collie](https://github.com/AltanS/collie) cover remote approval, where the right choice
depends on where you want to be pinged — not a decision a default set should make for you.

None of this is load-bearing: `init` just writes these lines into `plugins.list`. Edit it,
or skip `init` and build your own with `add`.

## Design notes

- **Rust, and nearly dependency-free.** The manager is orchestration — shelling out to
  the herdr CLI — which std covers, including a small hand-written JSON reader for
  `plugin list --json`. The one dependency is `crossterm`, because std cannot put a
  terminal into raw mode and the manage pane needs it. Not ratatui: the UI is a list
  with a status column, and ratatui costs 70 crates against crossterm's 19.
- **Never parse human output.** All state comes from `herdr plugin list --json`.
- **Long operations leave the TUI** rather than being redrawn as in-pane progress, so
  a failing plugin build shows you its actual output.

## License

MIT
