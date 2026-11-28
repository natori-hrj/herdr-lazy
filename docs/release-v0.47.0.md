# herdr-lazy v0.47.0 — Resource Safety & Reliability

This release contains [Resource Budgets (#146)](https://github.com/natori-hrj/herdr-lazy/pull/146).
Requires Herdr 0.9.0 or later. Run `herdr-lazy --version` or `herdr-lazy -V`
to identify your installed build.

## Response and input limits

- Captured Herdr CLI responses have an 8 MiB limit **per stream** and a shared
  15-second response deadline by default. Machine action acknowledgements use
  a three-second deadline; a timeout leaves the remote outcome unknown.
- Once both streams close, exit polling backs off from 200 µs to at most 5 ms,
  within the remaining response deadline. On failure the capture path attempts
  to terminate and reap the directly started child. It does not traverse the
  process tree, and cleanup can extend past the response deadline.
- The bounded local text readers limit plugin lists, generated locks, workspace
  profiles, and binding configuration to 64 KiB. Manage reports plugin-list
  read failures. The `probe` entry count can still show zero when it cannot
  read a list or lock.

## Generated locks and history

Cooperating readers and writers share a guard, and updates validate the active
lock and its history before publishing. Unsupported aliases are rejected.
Publication replaces individual files and attempts to roll back changes on
failure; the lock family is not a crash-atomic transaction.

On Linux and macOS, publication preserves ordinary mode bits, rejects stages whose
owner or group differs from the source, and refuses unsupported special modes
and extended ACLs. On Windows, publication checks the owner's identity and
DACL against the stage's creation policy; it does not copy arbitrary security
descriptors or other metadata. These checks assume a trusted, stable parent
directory and do not cover external writers.

## Performance

Linux PTY first-frame measurements used 30 interleaved runs per size and
fixture-backed Herdr inventory. The Resource Budgets column was measured on
the approved [code candidate](https://github.com/natori-hrj/herdr-lazy/commit/03d64956984abfd62b603f47cd9965ad4d070243)
before stamping the version.

| Inventory rows | v0.46.0 median | Resource Budgets median |
| ---: | ---: | ---: |
| 1,000 | 3.89 ms | 5.35 ms |
| 2,108 | 7.28 ms | 11.03 ms |
| 10,000 | 30.60 ms | 46.09 ms |

The 10,000-row fixture took 15.49 ms longer at the median. The adaptive wait
reduces exit polling after EOF; the measurements do not show an overall
speedup.

## Platforms and recordings

The [v0.46.0 demo](release-v0.46.0.md#terminal-recording) remains the walkthrough
for the existing UI. The [recording guide](demo.md) identifies the checkout
used to make those assets.

The release archive and SHA-256 sidecar format is unchanged. The Windows
source/build fingerprint issue [#148](https://github.com/natori-hrj/herdr-lazy/issues/148)
remains open; if the expected prebuilt asset cannot be used, the installer
falls back to a source build.

See the [changelog](../CHANGELOG.md) and [installation instructions](../README.md#install).
