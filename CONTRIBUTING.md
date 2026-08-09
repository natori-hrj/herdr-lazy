# Contributing

Contributions are welcome — issues, discussions, and pull requests alike. This document
says what happens to each, so you can tell in advance whether an idea is likely to land.

## Where to start

- **Discussions** — ideas, questions, "would you consider…", showing what you built. Nothing
  is too vague for a discussion. Start here if you are unsure.
- **Issues** — a concrete bug, or a change specific enough to act on. If an issue turns out
  to be open-ended, it will be moved to a discussion; that is not a rejection.
- **Pull requests** — welcome, including from first-time contributors. For anything larger
  than a fix, open an issue or discussion first so you do not write code that gets turned
  down for reasons you could not have known.

Everything gets read. Nothing is merged unreviewed, including trivial changes — this tool
installs and uninstalls other people's software on other people's machines, and a careless
change to the matcher or to `--prune` can delete work.

## The bar for a pull request

- `cargo test`, `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` pass.
  CI runs these plus the MSRV build; it must be green.
- New behaviour comes with a test. Bug fixes come with a test that fails without the fix.
- Comments explain *why*, not what. If a line looks redundant and is not, say why — the
  leading `./` in `herdr-plugin.toml` is the standing example.
- No new dependencies without discussion. The project has exactly one (`crossterm`, because
  std cannot set raw terminal mode) and each addition needs to clear the same bar.

Things that are held to a higher standard, because getting them wrong destroys user data:

- `Installed::matches` and the strong/weak distinction
- anything reached by `--prune` or `uninstall`
- `scripts/fetch-or-build.sh`, especially the checksum path

## Adding a plugin to the default set

This is the most common request, so the criteria are explicit. `DEFAULT_BUNDLE` is a curated
opinion, not a directory — the marketplace already lists everything. In order:

1. **It installs cleanly for a new user.** A plugin whose build is a bare
   `cargo build --release` fails under herdr's build PATH, which excludes `~/.cargo/bin`.
   Ship a prebuilt binary, or have no build step.
2. **It does not overlap something already in the set.** Two plugins that both open a file
   pane is a worse default than one. Overlap is a reason to *swap*, not to add.
3. **It is either widely used, or it covers a gap nothing else does.** Popularity is
   evidence, not a requirement.
4. **A default set should not make a personal choice for you.** Where the right answer
   depends on the user's setup — which notifier, which editor — the set stays out of it.

A "no" here is usually about the set, not the plugin. `herdr-lazy add owner/repo` works for
anything, and the default list is three lines of `src/main.rs` for anyone who disagrees.

Self-submissions are fine and are judged the same way. Note that the current set includes
plugins by this project's author, which is a fair thing to point at — the criteria above are
the answer, and they apply to those entries too.

## Adding a plugin to the extras

Extras are opt-in categories — `init --extras worktree` — for capabilities the default set
deliberately leaves out because the right answer depends on you: which worktree tool, which
notifier. An extra is proposed by a pull request and judged against a checklist, the same way
a default-set entry is. It differs from the default-set bar in one direction only: because an
extra is opt-in, it is allowed to be an opinionated, personal choice.

An extra earns its place when:

1. **It installs cleanly.** The same rule as the default set, and just as non-negotiable — a
   prebuilt binary or no build step. An install that fails under herdr's build PATH is not
   made acceptable by being opt-in.
2. **It is one coherent capability, not a category dump.** The unit is `worktrunk`, not "all
   the worktree plugins". Naming an extra after a single job is what keeps `--extras worktree`
   from installing three things that do the same thing.
3. **Within a category, overlapping plugins are alternatives, not a stack.** If two plugins
   both manage worktrees, they are two extras you choose between; enabling one must not pull
   in the other. This is the default set's "swap, not add", applied to the menu.
4. **Someone actually uses it, and it does what its description says.** The person proposing
   it is the obvious someone — "I built this and use it daily" is a good way to open the PR.
5. **Any external setup is stated up front.** If it needs a Slack app, an account, a running
   service, or a CLI installed separately, the extra's one-line description says so. The point
   of opting in is that the choice is informed.

Self-submissions are welcome and held to exactly this list, and so are the maintainer's own
plugins: being written by the author is not a reason to include something here, the same way
it is not for the default set.

If the grouping you want is a personal one — "the three things I install on every machine" —
you do not need this repository at all. A `.list` file in `extras/` beside your own plugin list
joins your menu directly; see the README. The bar above is for what ships to everyone, and it
would be the wrong thing to lower for a bundle only you use.

## Reporting a bug

The useful ones tend to include:

- `herdr-lazy probe` output — it prints the herdr version and the resolved paths, and
  `probe --raw` adds the full CLI payloads if the problem looks like a parsing one
- the contents of your `plugins.list`
- what you expected, and what happened

If herdr-lazy did something destructive — removed a plugin you wanted, or wrote a list you
did not ask for — say so plainly and it will be treated as a priority. `~/.config/herdr-lazy/`
may still hold an older copy of your list.

## Security

Do not open a public issue for a vulnerability in the install path (checksum handling,
downloaded binaries, anything that executes). Use GitHub's private vulnerability reporting on
the Security tab.

## Licence

By contributing you agree that your work is licensed under the MIT Licence, as the rest of
the project is.
