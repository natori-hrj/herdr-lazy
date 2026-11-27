# Reproduce the documentation demo

This recording uses the published herdr-lazy **v0.45.0** Linux x86-64 binary.
[VHS v0.12.1](https://github.com/charmbracelet/vhs/releases/tag/v0.12.1)
drives the actual TUI through ttyd and captures the browser's terminal viewport.
The tape records Manage, Plugin Details, the Command Palette, Marketplace
search, and an interactive theme preview.

## Regenerate the media

Run this on Linux x86-64 with Bash, curl, tar, unzip, sha256sum, Python 3,
Fontconfig, Chromium, FFmpeg, and Noto Sans CJK installed. Chromium and FFmpeg
are system dependencies. Use the same browser, FFmpeg and CJK font builds if
you need an exact rendering comparison.

```sh
./scripts/record-vhs.sh /tmp/herdr-lazy-media
```

The script downloads and verifies the official VHS, ttyd 1.7.7, JetBrains Mono
2.304 and herdr-lazy release assets against SHA-256 values pinned in the script.
It checks both executable versions and verifies that Fontconfig resolves
JetBrains Mono to the isolated copy. Chromium can use Noto Sans CJK for
characters missing from JetBrains Mono.

The replay starts with a fresh temporary home and XDG directories, and the
TUI gets an allowlisted environment. Plugin metadata and the cached Marketplace
are fictional fixtures. The app sees a stub Herdr and a private executable
search path, so the recording needs neither a running Herdr nor access to a
personal workspace. The downloads happen before recording, and VHS communicates
with ttyd over loopback. The temporary workspace is removed after the run.

## Tape and outputs

[`vhs/workflow.tape`](vhs/workflow.tape) sets a 1760×660 viewport, 28 px
padding, 20 px JetBrains Mono, a Catppuccin Mocha terminal palette, and a steady
cursor. All five screenshots are captured from that same take.

| Output | Interaction |
| --- | --- |
| `manage.png` | Open Manage on a fictional bundle |
| `details.png` | Press `l` for actions and plugin metadata |
| `command-palette.png` | Press Ctrl+P and type `market` |
| `marketplace.png` | Open Marketplace from the palette and type `worktree` |
| `theme-selection.png` | Return to Manage, press `T`, and preview Catppuccin Mocha |
| `demo.gif`, `demo.mp4` | Complete workflow, including the transitions |

The tape uses fixed pauses for readability. Those timings are independent of
[the PTY benchmarks](release-v0.45.0.md#performance).

Review the staged outputs before updating `docs/media/vhs/`. Check the entire
frame, including panel borders, the status symbols, Japanese text, the selected
row, and the cursor in each search field. Play or sample both videos and verify
their frame size and 20–30 second duration. After review, run
`./scripts/record-vhs.sh` to write to the tracked directory, or copy the reviewed
take there directly.
