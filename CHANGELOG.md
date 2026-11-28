# Changelog

## v0.47.0 — Resource Safety & Reliability

### Changed

- Bound captured Herdr CLI output and set response deadlines. After both streams close, check for the directly launched child's exit with adaptive backoff. On timeout or capture failure, stop local capture and attempt to terminate and reap that child.
- Bound reads of local plugin lists, locks, workspace profiles, and relevant configuration. Report unreadable or oversized inputs instead of treating them as an empty list in Manage.
- Check generated locks and their history for supported file types, identities, sizes and permission policies before publishing. Coordinate cooperating readers and writers and preserve supported permissions.

See [#146](https://github.com/natori-hrj/herdr-lazy/pull/146) and the [v0.47.0 release notes](docs/release-v0.47.0.md) for limits, platform boundaries, and the measured first-frame cost. Requires Herdr 0.9.0 or later.

## v0.46.0

### Added

- Combine Marketplace text search with category and installation filters. Use Tab or Shift+Tab to cycle categories, Ctrl+F to cycle installation filters, Ctrl+N to switch between stars and name order, and Ctrl+L to reset the search. [#139](https://github.com/natori-hrj/herdr-lazy/pull/139)

### Changed

- Normalize searchable Marketplace text when loading the catalog so repeated searches reuse it. [#136](https://github.com/natori-hrj/herdr-lazy/pull/136)
- Adapt Manage and Marketplace columns to the visible rows. Give the focused row a background while preserving status colors and improve the contrast of secondary text. [#138](https://github.com/natori-hrj/herdr-lazy/pull/138) [#141](https://github.com/natori-hrj/herdr-lazy/pull/141) [#143](https://github.com/natori-hrj/herdr-lazy/pull/143) [#144](https://github.com/natori-hrj/herdr-lazy/pull/144)
- Keep command palette results and key hints together, preview each theme on a plugin row, and make navigation and action hints more consistent across views. [#137](https://github.com/natori-hrj/herdr-lazy/pull/137) [#142](https://github.com/natori-hrj/herdr-lazy/pull/142) [#145](https://github.com/natori-hrj/herdr-lazy/pull/145)

### Fixed

- Clip and wrap terminal text at grapheme boundaries and measure its display width in cells. Keep selected text readable when terminal palette colors coincide. [#140](https://github.com/natori-hrj/herdr-lazy/pull/140)

See the [v0.46.0 release notes](docs/release-v0.46.0.md) for recordings, measurements, and verification coverage.
Requires Herdr 0.9.0 or later.

## v0.45.0

### Added

- Run `herdr-lazy --version` or `herdr-lazy -V` to print the installed build's version without running Herdr. [#125](https://github.com/natori-hrj/herdr-lazy/pull/125)
- Open the searchable Manage command palette with Ctrl+P and press Enter to open the highlighted view. Existing shortcuts remain available. [#107](https://github.com/natori-hrj/herdr-lazy/pull/107)
- Preview and save themes with `T` in Manage. Terminal colors remain the default. Catppuccin Mocha, Tokyo Night, Gruvbox, Nord, and custom color roles are available. `HERDR_LAZY_THEME` takes precedence over the saved preference. [#96](https://github.com/natori-hrj/herdr-lazy/pull/96) [#108](https://github.com/natori-hrj/herdr-lazy/pull/108)

### Changed

- Manage uses an adaptive list with text status labels. Wider panes show commits and notes. The keymap scrolls and consumes input while open. [#89](https://github.com/natori-hrj/herdr-lazy/pull/89) [#94](https://github.com/natori-hrj/herdr-lazy/pull/94)
- Plugin details scroll without moving the selected action and include available local repository, pin, commit, and Herdr compatibility information. [#101](https://github.com/natori-hrj/herdr-lazy/pull/101)
- Marketplace fits search and results to the terminal width and indicates installed and listed plugins. Cached results remain searchable during background refresh. The pane checks for refresh completion more often while a request is visible. [#93](https://github.com/natori-hrj/herdr-lazy/pull/93) [#100](https://github.com/natori-hrj/herdr-lazy/pull/100) [#102](https://github.com/natori-hrj/herdr-lazy/pull/102)

### Fixed

- Reject or bound malformed external responses before processing them. Keep the last valid Marketplace cache when a refresh fails or contains an invalid index. [#80](https://github.com/natori-hrj/herdr-lazy/pull/80) [#88](https://github.com/natori-hrj/herdr-lazy/pull/88)

### Security

- Limit JSON nesting to 64 containers, including arrays, objects, and unknown fields. [#120](https://github.com/natori-hrj/herdr-lazy/pull/120)
- Enforce the 8 MiB Marketplace limit when reading cached data and recover invalid caches during refresh. [#122](https://github.com/natori-hrj/herdr-lazy/pull/122)
- Share one deadline across downloader fallbacks: 15 seconds for GitHub and 20 seconds for Marketplace. Keep response limits and reap the directly started downloader on failure. [#123](https://github.com/natori-hrj/herdr-lazy/pull/123)

### Contributors

- Reproduce UI latency measurements in a fixture-backed PTY. Record offline demos of the Manage and Marketplace workflows, command palette, theme preview, and cached refresh. [#95](https://github.com/natori-hrj/herdr-lazy/pull/95) [#106](https://github.com/natori-hrj/herdr-lazy/pull/106)

Requires Herdr 0.9.0 or later.
