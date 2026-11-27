# Reproduce the documentation demos

The v0.46.0 tape drives a local herdr-lazy binary through ttyd and captures the
terminal viewport with [VHS v0.12.1](https://github.com/charmbracelet/vhs/releases/tag/v0.12.1).
It opens Manage and Help, finds Marketplace with the fuzzy query `mkt`, searches
for `worktree`, and refreshes the catalog in the background. It then combines
category, installation, and sort controls, resets the search, shows the empty
state, inspects a plugin with a Unicode description, and saves Nord.

## Record the v0.46.0 candidate

On Linux x86-64, install Bash, curl, tar, unzip, sha256sum, Python 3,
Fontconfig, Chromium, and FFmpeg. Build with the x86-64 Linux musl Rust
target installed.

```sh
cargo build --release --locked --target x86_64-unknown-linux-musl
./scripts/record-vhs.sh --candidate target/x86_64-unknown-linux-musl/release/herdr-lazy /tmp/herdr-lazy-v0460
```

Candidate mode checks the supplied executable's version against
`herdr-plugin.toml`. Omitting the output directory writes to
`docs/media/v0460/`. The [v0.46.0 tape](vhs/v0460-workflow.tape) uses
1280×720, 22 px JetBrains Mono, 24 px padding, Catppuccin Mocha, and a steady
cursor. The terminal palette stays fixed while the app changes its own theme.

| Files | Moment captured |
| --- | --- |
| `manage.png`, `help.png`, `manage-themed.png` | Manage, its keymap, and Manage after applying Nord |
| `command-palette.png`, `command-palette-search.png` | Available commands and the fuzzy query |
| `marketplace-catalog.png`, `marketplace.png`, `marketplace-refresh.png` | Full catalog, text search results, and in-progress refresh |
| `discovery-filtered.png`, `discovery-reset.png`, `discovery-empty.png` | Combined filters and name order, reset, and no results |
| `details.png` | Plugin actions and metadata with a description containing an emoji sequence, a decomposed accent, and Japanese text |
| `theme-catppuccin.png`, `theme-selection.png` | Theme previews on a plugin row |
| `demo.gif`, `demo.mp4` | Complete workflow |

Both modes download and check the pinned SHA-256 values for VHS, ttyd 1.7.7,
and JetBrains Mono 2.304, and verify that Fontconfig selects the temporary
JetBrains Mono installation. To use existing downloads, set `VHS_ASSET_DIR`
to their directory. Keep the filenames from the download URLs. The same
checksum checks apply. If using a custom `TMPDIR`, keep its path short enough
for Chromium's Unix-domain socket.

The fixture supplies 18 fictional Marketplace entries and a stub Herdr.
A local downloader stub serves the same catalog after a short delay so the
background refresh and its status are visible. The tape uses temporary home
and XDG directories. VHS and ttyd communicate over loopback. The script checks
that the app saved Nord in the fixture, then removes the temporary workspace.

The tape pauses for readability. See the [PTY performance measurements](release-v0.46.0.md#performance)
for timing data. Use the same Chromium, FFmpeg, and font fallback configuration
when comparing pixels or file sizes.

## Reproduce the v0.45.0 recording

The original invocation downloads the pinned published v0.45.0 binary,
checks its SHA-256 and version, and runs [the original tape](vhs/workflow.tape).

```sh
./scripts/record-vhs.sh /tmp/herdr-lazy-media
./scripts/record-vhs.sh /tmp/herdr-lazy-media-24 24
```

Omitting the output directory in this mode writes to `docs/media/vhs/`.
[v0.45.0 release notes and recordings](release-v0.45.0.md) ·
[v0.45.0 visual comparison](vhs/visual-review.md)
