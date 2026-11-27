# Changelog

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
