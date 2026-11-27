# Reproduce the documentation demo

The tape drives the published herdr-lazy **v0.45.0** Linux x86-64 binary through
ttyd and captures the terminal viewport with [VHS v0.12.1](https://github.com/charmbracelet/vhs/releases/tag/v0.12.1).
It opens Manage, finds Marketplace with the fuzzy query `mkt`, narrows the
catalog with `worktree`, refreshes it in the background, inspects the first
plugin, and previews themes before applying Nord.

## Regenerate the media

On Linux x86-64, install Bash, curl, tar, unzip, sha256sum, Python 3,
Fontconfig, Chromium and FFmpeg. Use the same system browser and FFmpeg
builds when comparing pixels or file sizes across runs.

```sh
./scripts/record-vhs.sh /tmp/herdr-lazy-media
./scripts/record-vhs.sh /tmp/herdr-lazy-media-24 24   # compare 24 px type
```

The script downloads official VHS, ttyd 1.7.7, JetBrains Mono 2.304, and
herdr-lazy release binaries. It checks their pinned SHA-256 values, verifies
the VHS and application versions, and checks that Fontconfig selects the
temporary JetBrains Mono installation.

The plugin names, metadata, bundle and all 18 Marketplace entries are
fictional. Manage shows nine plugins with Installed, Disabled, Missing and
Unlisted states. The `worktree` search returns 12 entries. A local downloader
stub serves the same fixture after a short delay so the real background
refresh and its status are visible. The tape uses temporary home and XDG
directories and the app starts with an allowlisted environment. VHS and ttyd
communicate over loopback. The script verifies that the app saved Nord inside
the fixture, then removes the temporary workspace.

## Tape and outputs

[`vhs/workflow.tape`](vhs/workflow.tape) uses 1280×720, 22 px JetBrains Mono,
24 px padding, Catppuccin Mocha and a steady cursor. The terminal palette
stays fixed while the app previews and applies its own role colors.

| Files | Moment captured |
| --- | --- |
| `manage.png`, `manage-themed.png` | Manage before and after applying Nord |
| `command-palette.png`, `command-palette-search.png` | Available commands and the fuzzy query |
| `marketplace-catalog.png`, `marketplace.png`, `marketplace-refresh.png` | Full catalog, filtered results and in-progress refresh |
| `details.png` | Repository, revision, actions, event and key binding preview |
| `theme-catppuccin.png`, `theme-selection.png` | Different theme previews |
| `demo.gif`, `demo.mp4` | Complete workflow and transitions |

The tape uses fixed pauses for readability. Those timings are independent of
[the PTY benchmarks](release-v0.45.0.md#performance). Review the staged
materials at full size and at 800 px and 390 px before updating
`docs/media/vhs/`. The [comparison](vhs/visual-review.md) records the chosen
settings and current layout trade-offs. Running `./scripts/record-vhs.sh`
without an output argument writes to the tracked media directory.
