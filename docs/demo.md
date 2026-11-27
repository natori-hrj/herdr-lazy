# Reproduce the TUI demo

The default 24-second recording uses the binary you build in a 112×24 terminal.
Its plugin list, installed records, and Marketplace entries are fictional,
so it runs without Herdr, network access, or any personal configuration.

On Linux or macOS, build with Rust 1.78 or later and run the recorder with Python 3:

```sh
cargo build --release --locked
python3 scripts/record-demo.py target/release/herdr-lazy /tmp/herdr-lazy-demo.cast
```

Supply an unused output path. The recorder uses only the Python standard
library and exits if an expected screen never appears. An asciinema player
can replay the resulting cast, for example with `asciinema play /tmp/herdr-lazy-demo.cast`.
Playback pauses are fixed by the recorder so that readers have time to see the
screens. They are independent of app latency.

| Playback | View | Action |
| --- | --- | --- |
| 0–4 s | Manage | Review two installed plugins and one missing entry |
| 4–9 s | Detail | Press `l` to see the selected plugin's declared actions |
| 9–11 s | Manage | Press `Esc` to return |
| 11–21 s | Marketplace | Press `/` and type `worktree` to narrow the catalog |
| 21–24 s | Manage | Press `Esc` and then `q` |

For a PR screenshot, pause on the relevant playback frame and capture the
terminal window. Use the same terminal font, dimensions and palette for
before/after images. Keep screenshots of real workspaces free of private paths
and repository names.

### Theme screenshots

With v0.45.0 or later, use a fixed 80×24 frame for
comparable screenshots. Record each built-in palette and pause on the same
view and timestamp:

```sh
for theme in terminal catppuccin tokyo-night gruvbox nord; do
  python3 scripts/record-demo.py target/release/herdr-lazy \
    "/tmp/herdr-lazy-${theme}.cast" --columns 80 --theme "$theme"
done
```

Use 0.1 seconds for the first Manage frame or 16 seconds for the completed
Marketplace search. A pre-theme release can record the same screens with the
terminal's default palette. The cast records color escape sequences, so use a
true-color terminal when comparing presets.

### Command palette, theme picker, and cache refresh

These modes use features introduced in v0.45.0. Build that version or a later
checkout and pass its binary to the recorder:

```sh
cargo build --release --locked
for mode in palette theme cached-marketplace; do
  python3 scripts/record-demo.py target/release/herdr-lazy \
    "/tmp/herdr-lazy-${mode}.cast" --columns 80 --mode "$mode"
done
```

| Mode | Feature | Frames to compare |
| --- | --- | --- |
| `palette` | Command palette | At 2 s, Ctrl+P opens Commands. At 5 s the query narrows to Browse Marketplace, which opens at 9 s. |
| `theme` | Interactive theme picker | At 2 s, `T` opens the picker. At 5 s it previews Catppuccin and at 9 s Enter applies it to the fixture. |
| `cached-marketplace` | Adaptive Marketplace with background refresh | At 2 s the cached catalog is visible. At 5 s it shows the search results, at 9 s a refresh is in flight, and at 13 s the failed refresh leaves those results visible. |

The theme mode writes only inside the disposable fixture and cannot be combined
with `--theme`, which has environment precedence over the picker. The cached
Marketplace mode uses a two-hour-old fictional index and a local downloader that
returns a controlled error after the in-flight frame is captured. No network
request is made. These playback timings are chosen for readability.
