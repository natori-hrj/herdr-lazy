# Draft release and announcement copy

For maintainer review. Confirm the release version, links, and included changes
before publishing. The base text describes the current workflow. The candidate
section applies only to changes included in the selected release.

## GitHub release draft

**herdr-lazy {version}**

herdr-lazy lets you manage Herdr plugins from one pane. Use `/` to browse the
Marketplace, `l` to inspect a plugin's declared actions, and `?` to see the
keymap. The plain-text plugin list records your choices, and the lockfile records
the resolved commits for a reproducible setup.

The contributor guide includes a 24-second offline demo you can record against
your local build using fixture data.

Requires Herdr 0.9.0 or later:

```sh
herdr plugin install natori-hrj/herdr-lazy
```

Restart Herdr and open Manage with `prefix+shift+l` on a fresh setup, or use
`herdr plugin pane open --plugin herdr-lazy --entrypoint manage --focus`.

### Candidate changes to include after merge

- Manage adapts to terminal width, with explicit text statuses and a visible selection.
  Help scrolls in short panes.
- Marketplace adapts to narrow terminals, clips long queries, and keeps its result
  list usable while refreshing the index. On a failed refresh, a valid cached catalog
  stays available.
- Detail scrolls while preserving action focus. You can inspect the locally known
  repository, resolved commit, listed ref, and Herdr version compatibility.
- Terminal default, Catppuccin, Tokyo Night, Gruvbox, Nord, and custom color presets
  are available through `HERDR_LAZY_THEME`. If included in the verified build,
  `T` previews and saves a theme and Ctrl+P opens the command palette.
- The offline recorder has separate candidate-only flows for the command palette,
  theme picker, and cached Marketplace refresh. The default workflow remains
  available for the current release.

The candidate also bounds JSON nesting, Marketplace cache reads, response
sizes, and shared downloader deadlines. A failed refresh preserves a valid cache.

See [the final candidate report](release-v0.45.0.md) for the build SHA,
alternating local PTY comparison, recordings, and verification results. The
Marketplace first-frame measurement reflects access to cached results while
refresh runs. The completion measurement includes the simulated downloader.

## X draft

Bring your Herdr plugins to a new machine with a list and lockfile. In the new
herdr-lazy release, Marketplace shows cached results during refresh, Ctrl+P finds
Manage commands, and T previews themes.
https://github.com/natori-hrj/herdr-lazy

## Reddit draft

**Title**  herdr-lazy gives Herdr a plugin manager with a Marketplace browser and lockfile

I built herdr-lazy for people who want to browse and manage plugins from their
Herdr workspace. The Manage pane lets you find a plugin, add it to a plain-text
list, and choose when to install it. A lockfile records the installed commits
for reproducing a setup on another machine.

In this release, cached Marketplace results remain usable during a refresh.
Ctrl+P searches Manage commands, and T previews and saves a theme. The offline
recording shows each interaction with fictional plugin data.

It works with Herdr 0.9.0 or later. The repository has installation steps and a
short, reproducible offline demo. Feedback on the workflow and platform behavior
is welcome.

https://github.com/natori-hrj/herdr-lazy

## Herdr community draft

If you keep a collection of Herdr plugins across workspaces or machines, give
herdr-lazy a try. Open Manage, press `/` to browse the Marketplace, then use
`l` on an installed plugin for its declared actions. The list and lockfile are
plain text and can be kept with your dotfiles.

This release adds readable Marketplace rows in narrow panes, Ctrl+P command
search, and theme selection with T. If you use a small Herdr pane, reports on
keyboard navigation and terminal sizing would be particularly helpful.

The README covers fresh installs and existing setups. I would especially
appreciate reports from Linux and Windows users about the install and pane
behavior.

https://github.com/natori-hrj/herdr-lazy
