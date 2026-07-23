//! herdr-lazy — be lazy: a curated, batteries-included plugin distro & manager for herdr.
//!
//! Two layers:
//!   1. manager   — a declarative bundle file + `sync` to converge your install to it.
//!   2. distro    — `init` writes a curated default set so "install one, get everything".
//!
//! The whole thing is itself a herdr plugin: it drives the herdr CLI (via HERDR_BIN_PATH)
//! to install/list/uninstall the *other* plugins.
//!
//! Verified against herdr 0.7.4 (see `probe`, and HANDOFF.md):
//!   - `plugin list --json` is the machine-readable contract; we never parse the human output.
//!   - `plugin install --ref REF` gives native pinning, so a bundle entry is `owner/repo@ref`
//!     and the lockfile is genuinely reproducible. (An earlier draft assumed no pinning
//!     existed and planned to manage git checkouts by hand — that was wrong; don't rebuild it.)
//!
//!   - A github `source` is `{kind, owner, repo, resolved_commit, managed_path,
//!     installed_unix_ms}`. `owner` and `repo` are SEPARATE fields — nothing in the payload
//!     holds a joined "owner/repo", so `Installed::slug` assembles it. `resolved_commit` is
//!     what lets the lockfile record the commit actually installed.
//!
//! `Installed::matches` still grades Strong/Weak (a local link has no owner/repo at all, so
//! only its name can be compared), and `--prune` acts on Strong only.

mod browse;
mod github;
mod json;
mod registry;
mod ui;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Curated "batteries-included" default set — the distro layer.
///
/// Two criteria, applied in order: prefer what the ecosystem has already vetted, then fill
/// the gaps nothing else covers. Overlapping plugins are deliberately excluded rather than
/// stacked — two plugins that both open a file pane is a worse default than one.
///
/// A third criterion, learned the hard way: it has to actually install. herdr runs plugin
/// builds with a minimal PATH that excludes `~/.cargo/bin`, so a plugin whose build is a bare
/// `cargo build --release` fails on machines where Rust is installed and works fine in the
/// user's own shell. A default set must not hand a new user a failed install.
///
/// Excluded, and why (revisit if these change):
///   - `yuk1ty/herdr-spreader` (41★) — the better-known layout plugin, but its build is a bare
///     `cargo build` and it fails to install under herdr's build PATH (verified 2026-07-20).
///     herdr-plugin-workspace-manager does the same job with no build step at all, so it wins
///     on the criterion that matters most for a default.
///   - `dcolinmorgan/herdr-remote` (100★), `AltanS/collie` (63★) — remote approval overlaps
///     herdr-hail. All three are good; which fits depends on where you want to be pinged,
///     which is not something a default set should decide.
///
/// Edit freely — `herdr-lazy init` writes these into your bundle file, and nothing here is
/// load-bearing.
const DEFAULT_BUNDLE: &[&str] = &[
    // Proven in the ecosystem, and verified to install cleanly.
    "cloudmanic/herdr-plus",                    // projects + quick actions
    "smarzban/herdr-file-viewer",               // git-aware read-only file pane
    "persiyanov/herdr-reviewr",                 // comment on an agent's diff, send it back
    "razajamil/herdr-plugin-workspace-manager", // per-workspace tab/pane layouts; no build step
    // Gaps nothing else covers: keeping a human oriented across several running agents.
    "natori-hrj/herdr-triage",  // which agent needs you most
    "natori-hrj/herdr-green",   // did its tests pass when it finished
    "natori-hrj/herdr-standup", // what all your agents actually changed
];

fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

/// Must match `id` in herdr-plugin.toml — it is how we ask herdr about ourselves.
const PLUGIN_ID: &str = "herdr-lazy";

/// Where the bundle and lock live.
///
/// herdr sets `HERDR_PLUGIN_CONFIG_DIR` when it launches a plugin, but a user running the
/// binary from a shell has no such variable — and if the two disagree, `init` writes a bundle
/// the manage pane cannot see, and the pane reports "no plugin list" for a set that plainly
/// exists. So when the variable is absent, *ask herdr* where our config belongs rather than
/// inventing a second location.
///
/// Cached: this shells out, and it is consulted several times per run.
pub(crate) fn config_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        if let Ok(d) = env::var("HERDR_PLUGIN_CONFIG_DIR") {
            return PathBuf::from(d);
        }
        if let Ok((true, out, _)) = run_herdr(&["plugin", "config-dir", PLUGIN_ID]) {
            let p = out.trim();
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        // herdr is unreachable or we are not registered with it yet (fresh checkout).
        legacy_config_dir()
    })
    .clone()
}

/// Where an earlier version kept things, before the location was taken from herdr.
fn legacy_config_dir() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("herdr-lazy")
}

pub(crate) fn bundle_path() -> PathBuf {
    config_dir().join("plugins.list")
}

/// The lock sits beside the bundle, not in a state dir.
///
/// It is generated, but it is also the file you copy to another machine to reproduce a
/// setup — the same reasoning that puts Cargo.lock next to Cargo.toml. Keeping both in one
/// directory also means there is exactly one location to reason about.
fn lock_path() -> PathBuf {
    config_dir().join("plugins.lock")
}

fn ensure_parent(p: &Path) -> io::Result<()> {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Run a herdr subcommand, returning (success, stdout, stderr).
fn run_herdr(args: &[&str]) -> io::Result<(bool, String, String)> {
    let out = Command::new(herdr_bin()).args(args).output()?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

/// Read a simple list file: one entry per line, `#` comments and blanks ignored.
fn read_lines(p: &Path) -> Vec<String> {
    match fs::read_to_string(p) {
        Ok(s) => s
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect(),
        Err(_) => Vec::new(),
    }
}

pub(crate) fn desired_plugins() -> Vec<String> {
    migrate_legacy_bundle();
    read_lines(&bundle_path())
}

/// Move a bundle written by an earlier version into the location herdr gives us.
///
/// Only ever copies into an empty slot — if a bundle already exists at the real location,
/// the legacy file is left alone and nothing is overwritten. Copy rather than move, so a
/// mistake here cannot lose the user's list.
fn migrate_legacy_bundle() {
    let current = bundle_path();
    if current.exists() {
        return;
    }
    let legacy = legacy_config_dir().join("plugins.list");
    if !legacy.exists() || legacy == current {
        return;
    }
    let Ok(body) = fs::read_to_string(&legacy) else {
        return;
    };
    if ensure_parent(&current).is_err() || fs::write(&current, &body).is_err() {
        return;
    }
    println!(
        "moved your plugin list to the location herdr uses:\n  {} -> {}\n  (the old copy is \
         left in place; delete it when you are happy)",
        legacy.display(),
        current.display()
    );
}

/// "owner/repo" or "owner/repo/subdir" -> "repo"
fn repo_leaf(spec: &str) -> String {
    let parts: Vec<&str> = spec.split('/').collect();
    if parts.len() >= 2 {
        parts[1].to_string()
    } else {
        spec.to_string()
    }
}

/// A bundle entry: `owner/repo[/subdir][@ref]`.
///
/// herdr's `plugin install` takes `--ref REF`, so pinning is native — the `@ref` suffix maps
/// straight onto it. No git-checkout management of our own is needed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spec {
    /// `owner/repo[/subdir]` — what `install`/`uninstall` want as the positional arg.
    pub(crate) repo: String,
    /// Commit / tag / branch, if pinned.
    pub(crate) reference: Option<String>,
}

impl Spec {
    pub(crate) fn parse(line: &str) -> Spec {
        match line.split_once('@') {
            Some((repo, r)) if !repo.is_empty() && !r.is_empty() => Spec {
                repo: repo.trim().to_string(),
                reference: Some(r.trim().to_string()),
            },
            _ => Spec {
                repo: line.trim().to_string(),
                reference: None,
            },
        }
    }

    /// How it appears in the bundle/lockfile.
    pub(crate) fn display(&self) -> String {
        match &self.reference {
            Some(r) => format!("{}@{}", self.repo, r),
            None => self.repo.clone(),
        }
    }
}

/// One entry from `herdr plugin list --json`.
///
/// `Default` exists for tests: this grows a field whenever herdr exposes something new, and
/// without it every fixture in the suite needs editing for a field it does not care about.
#[derive(Debug, Clone, Default)]
pub(crate) struct Installed {
    pub(crate) plugin_id: String,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    pub(crate) source_kind: String,
    /// `owner/repo` rebuilt from `source.owner` + `source.repo`. herdr stores them as two
    /// separate fields, never as a joined slug, so this has to be assembled.
    pub(crate) slug: Option<String>,
    /// `source.installed_unix_ms` — when herdr fetched this. Compared against the
    /// marketplace's `pushedAt` to spot plugins that have moved since.
    pub(crate) installed_unix_ms: Option<u64>,
    /// `source.resolved_commit` — the exact commit herdr checked out. This is what makes a
    /// lockfile real: we can record what is actually installed, not merely what was asked for.
    pub(crate) resolved_commit: Option<String>,
    /// Every string value inside `source`, as a fallback for source kinds we have not seen
    /// (e.g. a plain clone URL) so an unknown shape degrades to a match attempt, not a miss.
    source_values: Vec<String>,
    /// What this plugin can actually do, straight from its manifest. A distro that installs
    /// seven plugins has to answer "what did I just get" — and herdr already tells us.
    pub(crate) description: String,
    /// `(id, title)` for each action, invokable via `plugin action invoke`.
    pub(crate) actions: Vec<(String, String)>,
    /// `(id, title, placement)` for each pane the plugin can open.
    pub(crate) panes: Vec<(String, String, String)>,
    /// Event names that trigger this plugin on their own.
    pub(crate) events: Vec<String>,
}

/// How confident we are that an installed plugin is the bundle entry.
///
/// This distinction is the safety mechanism: `sync` may *skip installing* on a weak match
/// (worst case: a redundant install attempt), but `--prune` may only *uninstall* on a strong
/// one. Getting it wrong in the prune direction destroys a plugin the user wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Match {
    /// `source` names this exact repo — authoritative.
    Strong,
    /// Only the plugin's display name lines up with the repo leaf. Plausible, not proof:
    /// a plugin's `name` comes from its manifest and need not track its repo name.
    Weak,
    None,
}

impl Installed {
    pub(crate) fn matches(&self, spec: &Spec) -> Match {
        let want = spec.repo.to_lowercase();

        // Authoritative: herdr's own record of which repo this came from.
        if let Some(slug) = &self.slug {
            if slug.to_lowercase() == want {
                return Match::Strong;
            }
            // A bundle entry may name a subdir (`owner/repo/plugins/x`) while `source` records
            // only `owner/repo`. Same repo, so still authoritative.
            if want.starts_with(&format!("{}/", slug.to_lowercase())) {
                return Match::Strong;
            }
        }

        for v in &self.source_values {
            let v = v.to_lowercase();
            if v == want {
                return Match::Strong;
            }
            // Clone URLs: https://github.com/owner/repo(.git), git@github.com:owner/repo.git
            let trimmed = v.strip_suffix(".git").unwrap_or(&v);
            if trimmed.ends_with(&format!("/{}", want)) || trimmed.ends_with(&format!(":{}", want))
            {
                return Match::Strong;
            }
        }
        if self.name.to_lowercase() == repo_leaf(&spec.repo).to_lowercase() {
            return Match::Weak;
        }
        Match::None
    }
}

/// Whether an installed plugin actually honours its bundle entry's pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PinState {
    /// Not pinned, or installed at exactly the pinned commit.
    Satisfied,
    /// Pinned to a commit, but a different one is installed. `sync` must repair this.
    Drifted { have: String },
    /// Pinned to a tag or branch. herdr resolves those to a commit at install time and never
    /// reports the original ref back, so there is nothing local to compare against. Reported,
    /// not repaired — reinstalling on every sync just to be sure would be worse.
    Unverifiable,
}

/// Does this ref look like a commit id (possibly abbreviated) rather than a tag or branch?
fn is_commit_ref(r: &str) -> bool {
    r.len() >= 7 && r.chars().all(|c| c.is_ascii_hexdigit())
}

pub(crate) fn pin_state(spec: &Spec, installed: &Installed) -> PinState {
    let pin = match &spec.reference {
        Some(r) => r,
        None => return PinState::Satisfied,
    };
    if !is_commit_ref(pin) {
        return PinState::Unverifiable;
    }
    match &installed.resolved_commit {
        // A local link has no commit to compare; nothing to enforce.
        None => PinState::Unverifiable,
        Some(have) => {
            let (have_l, pin_l) = (have.to_lowercase(), pin.to_lowercase());
            if have_l == pin_l || have_l.starts_with(&pin_l) {
                PinState::Satisfied
            } else {
                PinState::Drifted { have: have.clone() }
            }
        }
    }
}

/// Collect every string leaf in a JSON value (used to flatten a `source` object).
fn collect_strings(v: &json::Value, out: &mut Vec<String>) {
    match v {
        json::Value::Str(s) => out.push(s.clone()),
        json::Value::Arr(a) => a.iter().for_each(|x| collect_strings(x, out)),
        json::Value::Obj(m) => m.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

fn parse_plugin_list(stdout: &str) -> Result<Vec<Installed>, String> {
    let v = json::parse(stdout.trim()).map_err(|e| format!("could not parse JSON: {}", e))?;
    let plugins = v
        .path(&["result", "plugins"])
        .and_then(|p| p.as_array())
        .ok_or("no `result.plugins` array in output")?;

    Ok(plugins
        .iter()
        .map(|p| {
            let mut source_values = Vec::new();
            if let Some(src) = p.get("source") {
                collect_strings(src, &mut source_values);
            }
            let slug = match (
                p.path(&["source", "owner"]).and_then(|v| v.as_str()),
                p.path(&["source", "repo"]).and_then(|v| v.as_str()),
            ) {
                (Some(o), Some(r)) => Some(format!("{}/{}", o, r)),
                _ => None,
            };
            Installed {
                plugin_id: p.str_field("plugin_id").unwrap_or_default().to_string(),
                name: p.str_field("name").unwrap_or_default().to_string(),
                enabled: p.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true),
                source_kind: p
                    .path(&["source", "kind"])
                    .and_then(|k| k.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                description: p.str_field("description").unwrap_or_default().to_string(),
                actions: p
                    .get("actions")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|x| {
                                (
                                    x.str_field("id").unwrap_or_default().to_string(),
                                    x.str_field("title").unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                panes: p
                    .get("panes")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|x| {
                                (
                                    x.str_field("id").unwrap_or_default().to_string(),
                                    x.str_field("title").unwrap_or_default().to_string(),
                                    x.str_field("placement").unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                events: p
                    .get("events")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.str_field("on").map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                installed_unix_ms: p
                    .path(&["source", "installed_unix_ms"])
                    .and_then(|v| match v {
                        json::Value::Num(n) if *n >= 0.0 => Some(*n as u64),
                        _ => None,
                    }),
                slug,
                resolved_commit: p
                    .path(&["source", "resolved_commit"])
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string()),
                source_values,
            }
        })
        .collect())
}

/// Snapshot the installed set via `plugin list --json`.
pub(crate) fn installed_plugins() -> Result<Vec<Installed>, String> {
    match run_herdr(&["plugin", "list", "--json"]) {
        Ok((true, out, _)) => parse_plugin_list(&out),
        Ok((false, _, err)) => Err(format!("`herdr plugin list` failed: {}", err.trim())),
        Err(e) => Err(format!("could not run herdr: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Print a command's raw output between markers, so nothing is summarised away.
fn dump_block(out: &str, err: &str) {
    println!("---- raw output start ----");
    print!("{}", out);
    if !out.is_empty() && !out.ends_with('\n') {
        println!();
    }
    if !err.trim().is_empty() {
        println!("(stderr) {}", err.trim());
    }
    println!("---- raw output end ----");
}

/// The make-or-break check: can a plugin drive the herdr CLI, and what does
/// `plugin list` actually print? Run this first, on a machine with herdr.
fn cmd_probe() -> io::Result<()> {
    println!("herdr-lazy probe — verifying the plugin <-> herdr CLI bridge\n");
    println!("HERDR_BIN_PATH = {}", herdr_bin());
    println!("config dir     = {}", config_dir().display());
    println!(
        "  (from {})",
        if env::var("HERDR_PLUGIN_CONFIG_DIR").is_ok() {
            "HERDR_PLUGIN_CONFIG_DIR"
        } else {
            "`herdr plugin config-dir`, or the legacy default if herdr is unreachable"
        }
    );
    println!("bundle         = {}", bundle_path().display());
    println!("lock           = {}", lock_path().display());
    println!();

    // 1. Can we reach the herdr binary at all?
    match run_herdr(&["--version"]) {
        Ok((ok, out, err)) => {
            println!("[herdr --version] success={}", ok);
            if !out.trim().is_empty() {
                println!("  stdout: {}", out.trim());
            }
            if !err.trim().is_empty() {
                println!("  stderr: {}", err.trim());
            }
        }
        Err(e) => {
            println!("[herdr --version] could not launch: {}", e);
            println!("\nVERDICT: cannot invoke herdr. Set HERDR_BIN_PATH or run inside herdr.");
            return Ok(());
        }
    }
    println!();

    // 2. What does `plugin` actually expose?
    //
    // This used to only grep the help text for keywords like "install"/"list" and print
    // booleans. That hid the *flags* — `list --json` and `install --ref REF` both went
    // unnoticed, and we nearly built a text parser and a whole git-checkout pinning layer
    // that herdr already provides. Print the help verbatim; let a human read it.
    match run_herdr(&["plugin", "--help"]) {
        Ok((ok, out, err)) => {
            println!("[herdr plugin --help] success={}", ok);
            dump_block(&out, &err);
        }
        Err(e) => println!("[herdr plugin --help] could not run: {}", e),
    }
    println!();

    // 3. The list format we parse in `sync`. `--json` is the contract; the human output is
    //    shown only so a reader can sanity-check that the two agree.
    match run_herdr(&["plugin", "list", "--json"]) {
        Ok((ok, out, err)) => {
            println!("[herdr plugin list --json] success={}", ok);
            dump_block(&out, &err);
        }
        Err(e) => println!("[herdr plugin list --json] could not run: {}", e),
    }
    println!();

    match run_herdr(&["plugin", "list"]) {
        Ok((ok, out, err)) => {
            println!("[herdr plugin list] (human, for comparison) success={}", ok);
            dump_block(&out, &err);
        }
        Err(e) => println!("[herdr plugin list] could not run: {}", e),
    }

    println!(
        "\n>>> NOTE: `plugin list` reports no owner/repo — only plugin_id, name and source.\n\
         >>> If any plugin above was installed from github, paste its `source` object back:\n\
         >>> that is the field `sync --prune` must match bundle entries against."
    );
    Ok(())
}

/// Write the curated default bundle (the distro layer).
fn cmd_init(force: bool) -> io::Result<()> {
    let p = bundle_path();
    if p.exists() && !force {
        println!(
            "bundle already exists: {} (use `init --force` to overwrite)",
            p.display()
        );
        return Ok(());
    }
    ensure_parent(&p)?;
    let mut body = String::new();
    body.push_str("# herdr-lazy bundle — your declarative plugin set.\n");
    body.push_str("# One `owner/repo` per line. `#` starts a comment.\n");
    body.push_str("# Curated defaults below; edit to taste, then run `herdr-lazy sync`.\n\n");
    for d in DEFAULT_BUNDLE {
        body.push_str(d);
        body.push('\n');
    }
    fs::write(&p, body)?;
    println!("wrote curated default bundle -> {}", p.display());
    println!("edit it if you like, then run `herdr-lazy sync`.");
    Ok(())
}

fn cmd_list() -> io::Result<()> {
    let desired = desired_plugins();
    if desired.is_empty() {
        println!(
            "no plugin list at {} — run `herdr-lazy init`.",
            bundle_path().display()
        );
        return Ok(());
    }
    println!("desired plugins ({}):", desired.len());
    for d in &desired {
        println!("  - {}", d);
    }
    Ok(())
}

/// Converge the installed plugin set to the bundle.
/// Converge the installed plugin set to the list.
///
/// `targets` restricts the work to named `owner/repo` entries; empty means everything. The
/// lock is only rewritten on a full run — a targeted sync is a partial view of the world, and
/// writing the lock from it would drop every entry it did not look at.
/// Read the lockfile as a set of specs.
pub(crate) fn lock_specs() -> Vec<Spec> {
    read_lines(&lock_path())
        .iter()
        .map(|l| Spec::parse(l))
        .collect()
}

/// Put the machine back into the state the lockfile records.
///
/// `sync` converges to the *list*, which may float; `restore` converges to the *lock*, which
/// does not. That is the difference between "the plugins I asked for" and "the exact commits
/// that were installed when this lock was written" — and it is what makes a lock copied from
/// another machine actually usable, rather than something you paste into the list by hand.
///
/// Deliberately does not rewrite the lock: it is the input here, not the output.
pub(crate) fn cmd_restore(targets: &[&str]) -> io::Result<()> {
    let all = lock_specs();
    if all.is_empty() {
        println!(
            "no lockfile at {} — run `herdr-lazy sync` first, or copy one from another machine.",
            lock_path().display()
        );
        return Ok(());
    }
    let unpinned = all.iter().filter(|s| s.reference.is_none()).count();
    if unpinned > 0 {
        println!(
            "note: {}/{} lock entries have no commit; those are installed at whatever the \
             default branch points to now.",
            unpinned,
            all.len()
        );
    }
    converge(&all, targets, false, false)
}

pub(crate) fn cmd_sync(prune: bool, targets: &[&str]) -> io::Result<()> {
    let all: Vec<Spec> = desired_plugins().iter().map(|l| Spec::parse(l)).collect();
    if all.is_empty() {
        println!(
            "no plugin list at {} — run `herdr-lazy init` first.",
            bundle_path().display()
        );
        return Ok(());
    }
    converge(&all, targets, prune, true)
}

/// Install whatever in `all` is missing or has drifted from its pin.
///
/// `write_lock` is false for `restore`, whose input IS the lock — rewriting it there would let
/// a partial restore quietly redefine the thing being restored to.
/// What `sync` would have to do, without doing any of it.
///
/// Returns the bundle entries that are missing or drifted — the ones a converge would act on.
/// Used by `startup`, which must decide whether there is anything to do before making any
/// noise or touching the network.
fn pending_work(all: &[Spec], installed: &[Installed]) -> Vec<Spec> {
    all.iter()
        .filter(|spec| {
            let hit = installed
                .iter()
                .map(|p| (p, p.matches(spec)))
                .filter(|(_, m)| *m != Match::None)
                .max_by_key(|(_, m)| (*m == Match::Strong) as u8);
            match hit {
                None => true, // not installed
                Some((p, _)) => matches!(pin_state(spec, p), PinState::Drifted { .. }),
            }
        })
        .cloned()
        .collect()
}

/// herdr's `[[startup]]` hook: converge the machine to the list when herdr starts, but only
/// when there is a gap, and only for gaps that can be closed without a network round trip
/// per plugin or a surprising rebuild.
///
/// The constraint that shapes this: startup runs on every server start and live handoff, for
/// a human who did not ask for it right then. So it must be silent when nothing is wrong (the
/// common case), and it must not turn a routine `herdr` launch into a minutes-long install of
/// everything in a fresh list. It installs what is missing — that is the "I opened herdr on a
/// new machine and my plugins appeared" story — but it never prunes and never updates, because
/// those change a working setup rather than complete an incomplete one.
///
/// Opt-in: does nothing unless `auto_sync` is enabled, because a plugin that installs other
/// software when herdr starts is not something to turn on by surprise.
fn cmd_startup() -> io::Result<()> {
    if !auto_sync_enabled() {
        return Ok(()); // silent: the hook fires for everyone, most have not opted in
    }
    let all: Vec<Spec> = desired_plugins().iter().map(|l| Spec::parse(l)).collect();
    if all.is_empty() {
        return Ok(());
    }
    let installed = match installed_plugins() {
        Ok(v) => v,
        Err(_) => return Ok(()), // herdr not answering yet; try again next start
    };

    let pending = pending_work(&all, &installed);
    let missing: Vec<&Spec> = pending
        .iter()
        .filter(|spec| !installed.iter().any(|p| p.matches(spec) != Match::None))
        .collect();

    if missing.is_empty() {
        return Ok(()); // already converged, or the only gaps are drifted pins (left alone)
    }

    // Only install what is absent. A drifted pin is a deliberate-looking state that `sync`
    // repairs on request; silently rewriting it at every launch would be a surprise.
    println!(
        "herdr-lazy: installing {} plugin(s) declared in your list…",
        missing.len()
    );
    for spec in missing {
        let mut args = vec!["plugin", "install", spec.repo.as_str()];
        if let Some(r) = &spec.reference {
            args.push("--ref");
            args.push(r.as_str());
        }
        args.push("--yes");
        match run_herdr(&args) {
            Ok((true, _, _)) => println!("  installed {}", spec.display()),
            Ok((false, _, err)) => println!("  FAILED {}: {}", spec.display(), err.trim()),
            Err(e) => println!("  could not run herdr: {}", e),
        }
    }
    // Refresh the lock so it reflects what is now installed.
    if let Ok(after) = installed_plugins() {
        let _ = write_lock(&all, &after);
    }
    Ok(())
}

/// Is startup auto-sync turned on?
///
/// A one-line marker file next to the list, rather than a config format: herdr-lazy has no
/// config file, and inventing one for a single boolean is not worth it. Presence = on.
pub(crate) fn auto_sync_enabled() -> bool {
    config_dir().join("auto-sync").exists()
}

/// Flip startup auto-sync, returning the new state and a line to show the user.
///
/// Lives here rather than in the pane because the CLI and the pane must agree on what the
/// marker file means; two implementations of "is it on" would eventually disagree.
pub(crate) fn toggle_auto_sync() -> io::Result<(bool, String)> {
    let marker = config_dir().join("auto-sync");
    if marker.exists() {
        fs::remove_file(&marker)?;
        Ok((
            false,
            "auto-sync off — herdr start will not install anything".to_string(),
        ))
    } else {
        ensure_parent(&marker)?;
        fs::write(
            &marker,
            "startup auto-sync is on; delete this file to turn it off\n",
        )?;
        Ok((
            true,
            "auto-sync on — missing plugins install themselves when herdr starts".to_string(),
        ))
    }
}

fn cmd_auto_sync(arg: Option<&str>) -> io::Result<()> {
    let marker = config_dir().join("auto-sync");
    match arg {
        Some("on") => {
            ensure_parent(&marker)?;
            fs::write(
                &marker,
                "startup auto-sync is on; delete this file to turn it off\n",
            )?;
            println!("auto-sync on — herdr-lazy will install missing plugins when herdr starts.");
        }
        Some("off") => {
            let _ = fs::remove_file(&marker);
            println!("auto-sync off.");
        }
        _ => println!(
            "auto-sync is {}",
            if marker.exists() { "on" } else { "off" }
        ),
    }
    Ok(())
}

fn converge(all: &[Spec], targets: &[&str], prune: bool, write_the_lock: bool) -> io::Result<()> {
    let all: Vec<Spec> = all.to_vec();
    let desired: Vec<Spec> = if targets.is_empty() {
        all.clone()
    } else {
        for t in targets {
            if !all.iter().any(|s| s.repo == *t) {
                println!("! {} is not in your list — skipping", t);
            }
        }
        all.iter()
            .filter(|s| targets.iter().any(|t| *t == s.repo))
            .cloned()
            .collect()
    };
    if desired.is_empty() {
        println!("nothing to do.");
        return Ok(());
    }

    let installed = match installed_plugins() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return Ok(());
        }
    };

    let mut present = 0;
    let mut added = 0;
    let mut failed = 0;
    for spec in &desired {
        let hit = installed
            .iter()
            .map(|p| (p, p.matches(spec)))
            .filter(|(_, m)| *m != Match::None)
            .max_by_key(|(_, m)| (*m == Match::Strong) as u8);

        if let Some((p, m)) = hit {
            // Being installed is not enough when the entry is pinned: a plugin sitting at the
            // wrong commit satisfies "present" while violating the pin. Treat that as work to
            // do, not as converged — otherwise `sync` cannot actually reproduce a bundle.
            let drift = match pin_state(spec, p) {
                PinState::Drifted { have } => Some(have),
                _ => None,
            };

            if drift.is_none() {
                present += 1;
                let mut notes = Vec::new();
                if m == Match::Weak {
                    notes.push(format!(
                        "matched on name only — source says `{}`",
                        p.source_kind
                    ));
                }
                if let PinState::Unverifiable = pin_state(spec, p) {
                    notes.push(
                        "pinned to a non-commit ref — cannot verify locally; \
                         pin a commit for a checkable guarantee"
                            .to_string(),
                    );
                }
                // Installed but disabled satisfies the bundle only nominally: herdr will not
                // run it. Say so, or `sync` reports success for a plugin that does nothing.
                if !p.enabled {
                    notes.push(format!(
                        "DISABLED — `herdr plugin enable {}` to activate",
                        p.plugin_id
                    ));
                }
                let suffix = if notes.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", notes.join("; "))
                };
                println!(
                    "= {} (present as {}){}",
                    spec.display(),
                    p.plugin_id,
                    suffix
                );
                continue;
            }

            println!(
                "↻ {} is at {} — restoring the pin",
                spec.repo,
                short(&drift.unwrap())
            );
        } else {
            println!("+ installing {} ...", spec.display());
        }

        let mut args = vec!["plugin", "install", spec.repo.as_str()];
        if let Some(r) = &spec.reference {
            args.push("--ref");
            args.push(r.as_str());
        }
        args.push("--yes");
        match run_herdr(&args) {
            Ok((true, _, _)) => {
                added += 1;
                println!("  ok");
            }
            Ok((false, out, err)) => {
                failed += 1;
                println!("  FAILED");
                if !out.trim().is_empty() {
                    println!("  stdout: {}", out.trim());
                }
                if !err.trim().is_empty() {
                    println!("  stderr: {}", err.trim());
                }
            }
            Err(e) => {
                failed += 1;
                println!("  could not run herdr: {}", e);
            }
        }
    }

    // Prune compares against the WHOLE list, never the filtered subset: an entry that was
    // filtered out is still wanted, and pruning against the subset would uninstall it.
    if prune {
        prune_extras(&all, &installed);
    }

    println!(
        "\nsummary: {} present, {} installed, {} failed, {} desired total",
        present,
        added,
        failed,
        desired.len()
    );
    // Re-query: the snapshot above predates this run's installs, so it has no commits for
    // them. Locking against it would silently record the new plugins as unpinned.
    if write_the_lock {
        let after = installed_plugins().unwrap_or_else(|e| {
            eprintln!("warning: could not re-read plugin list for the lock: {}", e);
            installed.clone()
        });
        write_lock(&all, &after)?;
    }
    Ok(())
}

/// Uninstall installed plugins that the bundle does not ask for.
///
/// Deliberately conservative: anything we are not certain about is *reported, not removed*.
/// A missed removal is an annoyance the user can finish by hand; a wrong removal destroys a
/// plugin they wanted. Skipped here are locally-linked plugins (herdr-lazy itself is usually
/// one, and `uninstall` is the wrong verb for them anyway) and weak name-only matches.
fn prune_extras(desired: &[Spec], installed: &[Installed]) {
    println!("\n-- prune --");
    let mut removed = 0;
    let mut kept = Vec::new();

    for p in installed {
        let best = desired
            .iter()
            .map(|s| p.matches(s))
            .max_by_key(|m| (*m == Match::Strong) as u8)
            .unwrap_or(Match::None);

        match best {
            Match::Strong => continue, // in the bundle
            Match::Weak => {
                kept.push(format!(
                    "{} — name matches a bundle entry but `source` does not confirm it",
                    p.plugin_id
                ));
                continue;
            }
            Match::None => {}
        }

        if is_self(p) {
            kept.push(format!(
                "{} — this is herdr-lazy itself; uninstall it with `herdr plugin uninstall` \
                 if you mean to",
                p.plugin_id
            ));
            continue;
        }

        if p.source_kind == "local" {
            kept.push(format!(
                "{} — locally linked ({}); use `herdr plugin unlink {}` if you mean it",
                p.plugin_id, p.source_kind, p.plugin_id
            ));
            continue;
        }

        println!("- uninstalling {} ...", p.plugin_id);
        match run_herdr(&["plugin", "uninstall", p.plugin_id.as_str()]) {
            Ok((true, _, _)) => {
                removed += 1;
                println!("  ok");
            }
            Ok((false, _, err)) => println!("  FAILED: {}", err.trim()),
            Err(e) => println!("  could not run herdr: {}", e),
        }
    }

    if !kept.is_empty() {
        println!("kept (not confidently extraneous):");
        for k in &kept {
            println!("  ! {}", k);
        }
    }
    println!("pruned {} plugin(s)", removed);
}

/// Is this installed plugin herdr-lazy itself?
///
/// While developing, herdr-lazy is a local link and prune skips it for that reason. Installed
/// normally it is an ordinary github plugin, and — not being in the user's list — it is
/// exactly the shape prune removes. So `sync --prune` would uninstall the tool mid-run,
/// deleting the directory of the running binary. Match on the plugin id, which herdr takes
/// from our own manifest.
fn is_self(p: &Installed) -> bool {
    is_self_id(&p.plugin_id)
}

pub(crate) fn is_self_id(plugin_id: &str) -> bool {
    plugin_id == PLUGIN_ID
}

/// herdr's config.toml.
///
/// Derived from `HERDR_SOCKET_PATH` (herdr sets it for every plugin, and the socket lives in
/// the config directory) rather than assuming `~/.config/herdr` — a user with XDG_CONFIG_HOME
/// set elsewhere would otherwise get a second config file that herdr never reads.
pub(crate) fn herdr_config_path() -> Option<PathBuf> {
    // Overridable so the write path can be exercised against a throwaway file. Without it the
    // only way to test binding is to point HERDR_SOCKET_PATH somewhere else, which also cuts
    // the CLI off from the running server — so the pane has nothing to bind.
    if let Ok(p) = env::var("HERDR_LAZY_CONFIG_PATH") {
        return Some(PathBuf::from(p));
    }
    let sock = env::var("HERDR_SOCKET_PATH").ok()?;
    let dir = PathBuf::from(sock).parent()?.to_path_buf();
    Some(dir.join("config.toml"))
}

/// Keys already bound in config.toml, as written.
///
/// A deliberately shallow read: find `key = "…"` lines. Parsing TOML properly would mean a
/// dependency, and this only has to answer "is this string already spoken for" — a question
/// where a false positive (refusing to bind) is harmless and a false negative would silently
/// shadow an existing binding.
fn bound_keys(config: &str) -> Vec<String> {
    config
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') || !l.starts_with("key") {
                return None;
            }
            let (_, rest) = l.split_once('=')?;
            let rest = rest.trim();
            rest.strip_prefix('"')?
                .split('"')
                .next()
                .map(|s| s.to_string())
        })
        .collect()
}

/// Would this key collide with something already bound?
///
/// Separate from `bind_action` so the pane can check before showing a confirmation screen —
/// asking someone to confirm a write that is going to be refused is a waste of their time.
pub(crate) fn check_bind_conflict(key: &str) -> Result<(), String> {
    let Some(path) = herdr_config_path() else {
        return Err("cannot locate herdr's config.toml (no HERDR_SOCKET_PATH)".to_string());
    };
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if bound_keys(&existing).iter().any(|k| k == key) {
        return Err(format!(
            "{} is already bound in config.toml — pick another, or edit it by hand",
            key
        ));
    }
    Ok(())
}

/// Append a `[[keys.command]]` binding for a plugin action.
///
/// Writing to someone's herdr config is the most invasive thing herdr-lazy does, so: refuse
/// on a conflict rather than shadowing, back the file up first, and mark what was added so it
/// can be found and removed by hand later.
/// What a binding will invoke.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BindTarget {
    /// A declared action: herdr has a first-class binding type for these.
    Action(String),
    /// A pane. herdr's `[[keys.command]]` has no type for opening one, so this binds the CLI
    /// command instead, as `type = "shell"`. Without this, the four plugins in the default
    /// set that expose only panes could not be bound at all.
    Pane(String),
}

impl BindTarget {
    /// The `type` and `command` fields for this target.
    pub(crate) fn toml_fields(&self, plugin_id: &str) -> (String, String) {
        match self {
            BindTarget::Action(id) => {
                ("plugin_action".to_string(), format!("{}.{}", plugin_id, id))
            }
            BindTarget::Pane(id) => (
                "shell".to_string(),
                format!(
                    "herdr plugin pane open --plugin {} --entrypoint {}",
                    plugin_id, id
                ),
            ),
        }
    }

    pub(crate) fn id(&self) -> &str {
        match self {
            BindTarget::Action(id) | BindTarget::Pane(id) => id,
        }
    }
}

pub(crate) fn bind_action(
    plugin_id: &str,
    target: &BindTarget,
    key: &str,
) -> Result<String, String> {
    let Some(path) = herdr_config_path() else {
        return Err("cannot locate herdr's config.toml (no HERDR_SOCKET_PATH)".to_string());
    };
    let existing = fs::read_to_string(&path).unwrap_or_default();

    if bound_keys(&existing).iter().any(|k| k == key) {
        return Err(format!(
            "{} is already bound in config.toml — pick another, or edit it by hand",
            key
        ));
    }

    // Back up before touching it. Same name every time: one restore point is what someone
    // needs after a mistake, and a directory of timestamped copies is its own mess.
    if !existing.is_empty() {
        let _ = fs::write(path.with_extension("toml.herdr-lazy-backup"), &existing);
    }

    let (kind, command) = target.toml_fields(plugin_id);
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&format!(
        "\n# added by herdr-lazy\n[[keys.command]]\nkey = \"{}\"\ntype = \"{}\"\ncommand = \"{}\"\n",
        key, kind, command
    ));
    fs::write(&path, body).map_err(|e| format!("could not write config.toml: {}", e))?;

    // Ask herdr to pick it up; without this the binding does nothing until the next restart.
    let reloaded = matches!(run_herdr(&["server", "reload-config"]), Ok((true, _, _)));
    Ok(if reloaded {
        format!("bound {} to {}.{}", key, plugin_id, target.id())
    } else {
        format!(
            "wrote {} to config.toml — run `herdr server reload-config` to activate",
            key
        )
    })
}

/// The `type` and `command` a binding would use — so the confirmation screen can show the
/// exact lines that will be written rather than a paraphrase of them.
pub(crate) fn bind_toml_fields(target: &BindTarget, plugin_id: &str) -> (String, String) {
    target.toml_fields(plugin_id)
}

/// Open one of a plugin's panes.
pub(crate) fn open_pane(plugin_id: &str, entrypoint: &str) -> String {
    match run_herdr(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        plugin_id,
        "--entrypoint",
        entrypoint,
    ]) {
        Ok((true, _, _)) => format!("opened {} ({})", entrypoint, plugin_id),
        Ok((false, out, err)) => {
            let msg = if err.trim().is_empty() { out } else { err };
            format!("could not open {}: {}", entrypoint, msg.trim())
        }
        Err(e) => format!("could not run herdr: {}", e),
    }
}

/// Run one of a plugin's declared actions.
///
/// The details view lists what a plugin can do; without this it could only describe them,
/// which is half an answer to "how do I use this thing".
pub(crate) fn invoke_action(plugin_id: &str, action_id: &str) -> String {
    match run_herdr(&[
        "plugin", "action", "invoke", action_id, "--plugin", plugin_id,
    ]) {
        Ok((true, _, _)) => format!("ran {}.{}", plugin_id, action_id),
        Ok((false, out, err)) => {
            let msg = if err.trim().is_empty() { out } else { err };
            format!("could not run {}: {}", action_id, msg.trim())
        }
        Err(e) => format!("could not run herdr: {}", e),
    }
}

/// Uninstall one plugin, applying the same rule `--prune` uses.
///
/// Returns a message rather than printing: the manage pane calls this while it owns the
/// screen. Refuses local links for the same reason prune does — they have no owner/repo, and
/// herdr-lazy is normally one, so this stops the pane uninstalling the tool running it.
pub(crate) fn uninstall_plugin(plugin_id: &str, source_kind: &str) -> String {
    if plugin_id == PLUGIN_ID {
        return format!(
            "{} is herdr-lazy itself — run `herdr plugin uninstall {}` from a shell instead",
            plugin_id, plugin_id
        );
    }
    if source_kind == "local" {
        return format!(
            "{} is a local link — use `herdr plugin unlink {}` if you really mean it",
            plugin_id, plugin_id
        );
    }
    match run_herdr(&["plugin", "uninstall", plugin_id]) {
        Ok((true, _, _)) => format!("uninstalled {}", plugin_id),
        Ok((false, out, err)) => {
            let msg = if err.trim().is_empty() { out } else { err };
            format!("could not uninstall {}: {}", plugin_id, msg.trim())
        }
        Err(e) => format!("could not run herdr: {}", e),
    }
}

/// Re-resolve unpinned bundle entries to their latest commit.
///
/// herdr has no `plugin update`; re-running `plugin install` is the update path — it reports
/// `replaces: <id> from github:owner/repo@<old sha>` and keeps the plugin's config dir. So
/// "update" is: install again without `--ref`, then diff the resolved commits.
///
/// Pinned entries (`owner/repo@ref`) are skipped by design. A pin is a statement that this
/// commit is the one you want; silently moving it would make the lockfile a lie. To move a
/// pin, edit the bundle.
pub(crate) fn cmd_update(targets: &[&str]) -> io::Result<()> {
    let desired: Vec<Spec> = desired_plugins().iter().map(|l| Spec::parse(l)).collect();
    if desired.is_empty() {
        println!(
            "no plugin list at {} — run `herdr-lazy init` first.",
            bundle_path().display()
        );
        return Ok(());
    }

    // Restrict to named plugins, if any were given.
    let selected: Vec<&Spec> = if targets.is_empty() {
        desired.iter().collect()
    } else {
        let picked: Vec<&Spec> = desired
            .iter()
            .filter(|s| targets.iter().any(|t| *t == s.repo))
            .collect();
        for t in targets {
            if !desired.iter().any(|s| s.repo == *t) {
                println!("! {} is not in the bundle — skipping", t);
            }
        }
        picked
    };
    if selected.is_empty() {
        println!("nothing to update.");
        return Ok(());
    }

    let before = match installed_plugins() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return Ok(());
        }
    };
    let commit_of = |set: &[Installed], spec: &Spec| -> Option<String> {
        set.iter()
            .find(|p| p.matches(spec) == Match::Strong)
            .and_then(|p| p.resolved_commit.clone())
    };

    let mut changed = 0;
    let mut unchanged = 0;
    let mut pinned = 0;
    let mut failed = 0;
    for spec in &selected {
        if spec.reference.is_some() {
            pinned += 1;
            println!("• {} (pinned — edit the bundle to move it)", spec.display());
            continue;
        }
        let old = commit_of(&before, spec);
        print!("↻ {} ... ", spec.repo);
        match run_herdr(&["plugin", "install", spec.repo.as_str(), "--yes"]) {
            Ok((true, _, _)) => {}
            Ok((false, out, err)) => {
                failed += 1;
                println!("FAILED");
                let msg = if err.trim().is_empty() { out } else { err };
                if !msg.trim().is_empty() {
                    println!("  {}", msg.trim());
                }
                continue;
            }
            Err(e) => {
                failed += 1;
                println!("could not run herdr: {}", e);
                continue;
            }
        }

        // Re-read rather than trusting the install output: `resolved_commit` is herdr's own
        // record, and it is what the lock will be written from.
        let now = installed_plugins().unwrap_or_default();
        let new = commit_of(&now, spec);
        match (&old, &new) {
            (Some(o), Some(n)) if o == n => {
                unchanged += 1;
                println!("up to date ({})", short(o));
            }
            (Some(o), Some(n)) => {
                changed += 1;
                println!("{} -> {}", short(o), short(n));
            }
            (None, Some(n)) => {
                changed += 1;
                println!("installed ({}) — was missing", short(n));
            }
            _ => {
                unchanged += 1;
                println!("done (no commit reported)");
            }
        }
    }

    println!(
        "\nsummary: {} updated, {} already current, {} pinned, {} failed",
        changed, unchanged, pinned, failed
    );

    let after = installed_plugins().unwrap_or(before);
    write_lock(&desired, &after)?;
    Ok(())
}

/// Abbreviate a commit for display, without assuming it is a 40-char sha (a `--ref` may be a
/// tag or branch name that herdr echoes back).
pub(crate) fn short(commit: &str) -> String {
    if commit.len() > 12 && commit.chars().all(|c| c.is_ascii_hexdigit()) {
        commit[..12].to_string()
    } else {
        commit.to_string()
    }
}

/// Record the desired set, including any `@ref` pins.
///
/// With herdr's native `install --ref`, a bundle whose entries are all pinned to commit SHAs
/// is genuinely reproducible across machines, which is the whole point of the lockfile.
/// Unpinned entries still float, and are flagged as such.
fn write_lock(desired: &[Spec], installed: &[Installed]) -> io::Result<()> {
    let p = lock_path();
    ensure_parent(&p)?;

    // Prefer the commit herdr actually checked out (`source.resolved_commit`) over the ref the
    // bundle asked for: a bundle may say `main`, but the lock must say which `main`. This is
    // what makes the lock reproducible rather than merely descriptive.
    let mut lines = Vec::new();
    let mut unresolved = 0;
    let mut drifted = Vec::new();
    for d in desired {
        let hit = installed.iter().find(|p| p.matches(d) == Match::Strong);
        // A commit pin that disagrees with what is installed means bundle and reality have
        // diverged. Record the truth (what is installed), but never let it pass silently:
        // a lock that quietly contradicts its bundle is worse than no lock.
        if let Some(p) = hit {
            if let PinState::Drifted { have } = pin_state(d, p) {
                drifted.push(format!(
                    "{} pins {} but {} is installed",
                    d.repo,
                    short(d.reference.as_deref().unwrap_or("")),
                    short(&have)
                ));
            }
        }
        match hit.and_then(|p| p.resolved_commit.clone()) {
            Some(c) => lines.push(format!("{}@{}", d.repo, c)),
            None => {
                unresolved += 1;
                lines.push(d.display());
            }
        }
    }

    let mut body = String::new();
    body.push_str("# herdr-lazy lock — resolved plugin set at last sync.\n");
    body.push_str("# Each `owner/repo@commit` reproduces exactly via `plugin install --ref`.\n");
    body.push_str("# Commits come from herdr's own `source.resolved_commit`.\n\n");
    for l in &lines {
        body.push_str(l);
        body.push('\n');
    }
    fs::write(&p, body)?;
    println!("wrote lock -> {}", p.display());
    if unresolved > 0 {
        println!(
            "note: {}/{} entries have no resolved commit (not installed, or a local link) \
             and are recorded unpinned.",
            unresolved,
            desired.len()
        );
    }
    if !drifted.is_empty() {
        println!("WARNING: the lock disagrees with the bundle's pins:");
        for d in &drifted {
            println!("  ! {}", d);
        }
        println!("  run `herdr-lazy sync` to restore the pinned commits.");
    }
    Ok(())
}

/// Add an entry to the list, returning what to tell the user.
///
/// Returns a message rather than printing, because the manage pane calls this while it owns
/// the screen — a stray `println!` there corrupts the display.
pub(crate) fn add_to_list(spec: &str) -> io::Result<String> {
    let p = bundle_path();
    if read_lines(&p).iter().any(|l| l.as_str() == spec) {
        return Ok(format!("{} is already in your list", spec));
    }
    ensure_parent(&p)?;
    let mut existing = fs::read_to_string(&p).unwrap_or_default();
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(spec);
    existing.push('\n');
    fs::write(&p, existing)?;
    Ok(format!("added {} to your list", spec))
}

/// Drop an entry from the list. Does NOT uninstall — that is `sync --prune`.
pub(crate) fn remove_from_list(spec: &str) -> io::Result<String> {
    let p = bundle_path();
    let Ok(content) = fs::read_to_string(&p) else {
        return Ok(format!("no plugin list at {}", p.display()));
    };
    let mut kept = String::new();
    let mut removed = false;
    for line in content.lines() {
        if line.trim() == spec {
            removed = true;
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    if !removed {
        return Ok(format!("{} is not in your list", spec));
    }
    fs::write(&p, kept)?;
    Ok(format!(
        "dropped {} from your list (still installed; `sync --prune` uninstalls it)",
        spec
    ))
}

fn cmd_add(spec: &str) -> io::Result<()> {
    println!("{}", add_to_list(spec)?);
    println!("run `herdr-lazy sync` to apply.");
    Ok(())
}

fn cmd_remove(spec: &str) -> io::Result<()> {
    println!("{}", remove_from_list(spec)?);
    Ok(())
}

fn print_help() {
    println!("herdr-lazy — be lazy: a curated plugin distro & manager for herdr\n");
    println!("USAGE: herdr-lazy <command>\n");
    println!("  probe             verify the plugin <-> herdr CLI bridge (run this first)");
    println!("  init [--force]    write the curated default bundle (the distro layer)");
    println!("  list              show desired plugins");
    println!("  install [<repo>…] install what is missing, restore drifted pins");
    println!("  sync [--prune]    the same, plus --prune to remove what is not listed");
    println!("  update [<repo>…]  re-resolve unpinned entries to their latest commit");
    println!("  restore [<repo>…] put plugins back to the commits in the lockfile");
    println!("  ui                open the manage pane (also `manage`)");
    println!("  add <owner/repo>  add a plugin to the bundle");
    println!("  remove <owner/repo>  remove a plugin from the bundle");
    println!("  lock              write the lockfile from the current bundle");
    println!("  auto-sync [on|off]  install missing plugins automatically when herdr starts");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    let rest: Vec<&str> = args.iter().skip(2).map(|s| s.as_str()).collect();

    let result = match cmd {
        "probe" => cmd_probe(),
        "startup" => cmd_startup(),
        "auto-sync" => cmd_auto_sync(rest.first().copied()),
        "init" => cmd_init(rest.contains(&"--force")),
        "list" => cmd_list(),
        // `install` is what people look for; `sync` is what the operation is. Both, rather
        // than choosing and leaving the other as a dead end.
        "install" | "sync" => {
            let targets: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|a| !a.starts_with("--"))
                .collect();
            cmd_sync(rest.contains(&"--prune"), &targets)
        }
        "ui" | "manage" => ui::run(),
        "restore" => {
            let targets: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|a| !a.starts_with("--"))
                .collect();
            cmd_restore(&targets)
        }
        "update" => {
            let targets: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|a| !a.starts_with("--"))
                .collect();
            cmd_update(&targets)
        }
        "add" => match rest.first() {
            Some(spec) => cmd_add(spec),
            None => {
                eprintln!("usage: herdr-lazy add <owner/repo>");
                Ok(())
            }
        },
        "remove" => match rest.first() {
            Some(spec) => cmd_remove(spec),
            None => {
                eprintln!("usage: herdr-lazy remove <owner/repo>");
                Ok(())
            }
        },
        "lock" => {
            let specs: Vec<Spec> = desired_plugins().iter().map(|l| Spec::parse(l)).collect();
            let installed = installed_plugins().unwrap_or_else(|e| {
                eprintln!("warning: {} — locking without resolved commits", e);
                Vec::new()
            });
            write_lock(&specs, &installed)
        }
        _ => {
            print_help();
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from `herdr plugin list --json` (herdr 0.7.4), trimmed of long arrays.
    const LINKED_LOCAL: &str = r#"{"id":"cli:plugin","result":{"plugins":[{"actions":[{"command":["target/release/herdr-lazy","init"],"contexts":["workspace"],"id":"init","title":"Lazy: install curated defaults"}],"build":[{"command":["cargo","build","--release"]}],"description":"Be lazy","enabled":true,"manifest_path":"/Users/n/work/herdr-lazy/herdr-plugin.toml","min_herdr_version":"0.7.0","name":"herdr-lazy","platforms":["macos"],"plugin_id":"herdr-lazy","plugin_root":"/Users/n/work/herdr-lazy","source":{"kind":"local"},"version":"0.1.0"}],"type":"plugin_list"}}"#;

    const EMPTY: &str = r#"{"id":"cli:plugin","result":{"plugins":[],"type":"plugin_list"}}"#;

    fn installed(name: &str, kind: &str, source_values: &[&str]) -> Installed {
        Installed {
            plugin_id: format!("test.{}", name),
            name: name.to_string(),
            enabled: true,
            source_kind: kind.to_string(),
            slug: None,
            resolved_commit: None,
            source_values: source_values.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// As herdr records a github install: `owner` and `repo` are separate fields.
    fn from_github(owner: &str, repo: &str) -> Installed {
        Installed {
            plugin_id: repo.to_string(),
            name: repo.to_string(),
            enabled: true,
            source_kind: "github".to_string(),
            slug: Some(format!("{}/{}", owner, repo)),
            resolved_commit: Some("10e93033263549600e75119c5617dac48137d011".to_string()),
            source_values: vec![owner.to_string(), repo.to_string(), "github".to_string()],
            ..Default::default()
        }
    }

    /// Verbatim `source` for a real github install (herdr 0.7.4). `owner` and `repo` are
    /// SEPARATE fields — there is no joined "owner/repo" string anywhere in the payload.
    /// Flattening source strings and comparing to "owner/repo" therefore never matches, which
    /// is exactly the bug this test pins down: it silently degraded every github plugin to a
    /// weak name-only match, and weak matches are invisible to `--prune`.
    const GITHUB_INSTALL: &str = r#"{"id":"cli:plugin","result":{"plugins":[{"enabled":true,"name":"herdr-file-viewer","plugin_id":"herdr-file-viewer","plugin_root":"/c/plugins/github/herdr-file-viewer-c993314e2614","source":{"installed_unix_ms":1784546174080,"kind":"github","managed_path":"/c/plugins/github/herdr-file-viewer-c993314e2614","owner":"smarzban","repo":"herdr-file-viewer","resolved_commit":"10e93033263549600e75119c5617dac48137d011"},"version":"1.13.0"}],"type":"plugin_list"}}"#;

    #[test]
    fn parses_github_source_shape() {
        let ps = parse_plugin_list(GITHUB_INSTALL).expect("real github payload should parse");
        assert_eq!(ps[0].slug.as_deref(), Some("smarzban/herdr-file-viewer"));
        assert_eq!(
            ps[0].resolved_commit.as_deref(),
            Some("10e93033263549600e75119c5617dac48137d011")
        );
        assert_eq!(ps[0].source_kind, "github");
    }

    #[test]
    fn github_install_is_a_strong_match() {
        let ps = parse_plugin_list(GITHUB_INSTALL).unwrap();
        assert_eq!(
            ps[0].matches(&Spec::parse("smarzban/herdr-file-viewer")),
            Match::Strong,
            "owner+repo must be joined into a slug, or --prune can never act on github plugins"
        );
    }

    #[test]
    fn slug_match_beats_a_same_named_repo_from_another_owner() {
        let p = from_github("smarzban", "herdr-file-viewer");
        assert_eq!(
            p.matches(&Spec::parse("impostor/herdr-file-viewer")),
            Match::Weak
        );
        assert_eq!(
            p.matches(&Spec::parse("smarzban/herdr-file-viewer")),
            Match::Strong
        );
    }

    #[test]
    fn subdir_spec_matches_its_parent_repo_slug() {
        let p = from_github("owner", "repo");
        assert_eq!(
            p.matches(&Spec::parse("owner/repo/plugins/x")),
            Match::Strong
        );
        // ...but a different repo that merely shares a prefix must not.
        assert_eq!(p.matches(&Spec::parse("owner/repo-other")), Match::None);
    }

    #[test]
    fn parses_real_list_output() {
        let ps = parse_plugin_list(LINKED_LOCAL).expect("real payload should parse");
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].plugin_id, "herdr-lazy");
        assert_eq!(ps[0].name, "herdr-lazy");
        assert_eq!(ps[0].source_kind, "local");
        assert!(ps[0].enabled);
        assert_eq!(ps[0].source_values, vec!["local".to_string()]);
        assert_eq!(ps[0].slug, None, "a local link has no owner/repo");
        assert_eq!(ps[0].resolved_commit, None);
    }

    #[test]
    fn parses_empty_list() {
        assert!(parse_plugin_list(EMPTY).unwrap().is_empty());
    }

    #[test]
    fn rejects_unparseable_output() {
        assert!(parse_plugin_list("No plugins installed.").is_err());
        assert!(parse_plugin_list(r#"{"result":{}}"#).is_err());
    }

    fn at_commit(commit: Option<&str>) -> Installed {
        let mut p = from_github("owner", "repo");
        p.resolved_commit = commit.map(|c| c.to_string());
        p
    }

    /// The bug this pins down: an entry pinned to one commit, but sitting at another, was
    /// reported "present" and never repaired, so `sync` could not actually reproduce a bundle.
    /// `startup` acts only on this set, so it must be exactly "missing or drifted" — an
    /// installed, on-pin plugin appearing here would make every launch do needless work.
    /// `from_github` installs commit `10e9303…`; a pin to any other commit is drift.
    #[test]
    fn pending_work_is_only_missing_and_drifted() {
        const INSTALLED: &str = "10e93033263549600e75119c5617dac48137d011";
        let desired: Vec<Spec> = [
            "owner/here".to_string(),
            "owner/gone".to_string(),
            "owner/moved@deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(),
            format!("owner/pinned-ok@{}", INSTALLED),
        ]
        .iter()
        .map(|l| Spec::parse(l))
        .collect();
        let installed = vec![
            from_github("owner", "here"),
            from_github("owner", "moved"), // installed at INSTALLED, pinned elsewhere -> drift
            from_github("owner", "pinned-ok"), // installed at exactly its pin -> satisfied
        ];
        let pending = pending_work(&desired, &installed);
        let repos: Vec<String> = pending.iter().map(|s| s.repo.clone()).collect();
        assert!(
            repos.iter().any(|r| r == "owner/gone"),
            "missing is pending"
        );
        assert!(
            repos.iter().any(|r| r == "owner/moved"),
            "drifted pin is pending"
        );
        assert!(
            !repos.iter().any(|r| r == "owner/here"),
            "a satisfied entry is not pending"
        );
        assert!(
            !repos.iter().any(|r| r == "owner/pinned-ok"),
            "an entry sitting on its pin is not pending"
        );
    }

    #[test]
    fn nothing_pending_when_everything_matches() {
        let desired = vec![Spec::parse("owner/a"), Spec::parse("owner/b")];
        let installed = vec![from_github("owner", "a"), from_github("owner", "b")];
        assert!(pending_work(&desired, &installed).is_empty());
    }

    #[test]
    fn a_pinned_entry_at_the_wrong_commit_is_drift() {
        let spec = Spec::parse("owner/repo@a8f86ec4103bc367b52e547b492483f3b792a952");
        let p = at_commit(Some("f32b0825f12543c1d03e54fb10d1741c40d66cdc"));
        assert_eq!(
            pin_state(&spec, &p),
            PinState::Drifted {
                have: "f32b0825f12543c1d03e54fb10d1741c40d66cdc".to_string()
            }
        );
    }

    #[test]
    fn a_pinned_entry_at_the_right_commit_is_satisfied() {
        let sha = "a8f86ec4103bc367b52e547b492483f3b792a952";
        assert_eq!(
            pin_state(
                &Spec::parse(&format!("owner/repo@{}", sha)),
                &at_commit(Some(sha))
            ),
            PinState::Satisfied
        );
        // An abbreviated pin is satisfied by the full commit it prefixes.
        assert_eq!(
            pin_state(&Spec::parse("owner/repo@a8f86ec"), &at_commit(Some(sha))),
            PinState::Satisfied
        );
        // ...but a similar-looking prefix that does not match is still drift.
        assert!(matches!(
            pin_state(&Spec::parse("owner/repo@a8f86ff"), &at_commit(Some(sha))),
            PinState::Drifted { .. }
        ));
    }

    #[test]
    fn an_unpinned_entry_never_drifts() {
        assert_eq!(
            pin_state(&Spec::parse("owner/repo"), &at_commit(Some("f32b0825f125"))),
            PinState::Satisfied
        );
    }

    /// Tags and branches resolve to a commit at install time and are not echoed back, so there
    /// is nothing to compare — say so rather than reinstalling on every sync.
    #[test]
    fn tag_and_branch_pins_are_unverifiable() {
        for r in ["v1.13.0", "main", "release-2"] {
            assert_eq!(
                pin_state(
                    &Spec::parse(&format!("owner/repo@{}", r)),
                    &at_commit(Some("f32b0825f125"))
                ),
                PinState::Unverifiable,
                "{} should be unverifiable",
                r
            );
        }
        // A local link has no commit at all.
        assert_eq!(
            pin_state(&Spec::parse("owner/repo@a8f86ec4103b"), &at_commit(None)),
            PinState::Unverifiable
        );
    }

    #[test]
    fn commit_refs_are_told_apart_from_names() {
        assert!(is_commit_ref("a8f86ec"));
        assert!(is_commit_ref("a8f86ec4103bc367b52e547b492483f3b792a952"));
        assert!(!is_commit_ref("v1.0.0"));
        assert!(!is_commit_ref("main"));
        assert!(!is_commit_ref("abc123"), "too short to be unambiguous");
        // `deadbee` is hex and 7 chars — a legitimate abbreviated commit, and also a plausible
        // branch name. Treating it as a commit is the safe reading: it gets verified.
        assert!(is_commit_ref("deadbee"));
    }

    #[test]
    fn short_abbreviates_shas_but_not_tags() {
        assert_eq!(
            short("10e93033263549600e75119c5617dac48137d011"),
            "10e930332635"
        );
        // A `--ref` may be a tag or branch; truncating those would be misleading.
        assert_eq!(short("v1.13.0"), "v1.13.0");
        assert_eq!(short("release-candidate-2"), "release-candidate-2");
        assert_eq!(short("abc123"), "abc123");
    }

    /// `update` must leave pinned entries alone: a pin says "this commit", and moving it
    /// silently would make the lockfile disagree with the bundle.
    #[test]
    fn pinned_entries_are_distinguishable_from_floating_ones() {
        let bundle = ["owner/a", "owner/b@9f3c1ab", "owner/c"];
        let specs: Vec<Spec> = bundle.iter().map(|l| Spec::parse(l)).collect();
        let floating: Vec<&str> = specs
            .iter()
            .filter(|s| s.reference.is_none())
            .map(|s| s.repo.as_str())
            .collect();
        assert_eq!(floating, vec!["owner/a", "owner/c"]);
    }

    /// herdr has a first-class binding type for actions but none for panes, so panes go
    /// through `type = "shell"` and the CLI. Getting this wrong writes a config line herdr
    /// silently ignores — the user presses the key and nothing happens, with no error.
    #[test]
    fn actions_and_panes_produce_different_bindings() {
        let (kind, cmd) =
            BindTarget::Action("projects".into()).toml_fields("cloudmanic.herdr-plus");
        assert_eq!(kind, "plugin_action");
        assert_eq!(cmd, "cloudmanic.herdr-plus.projects");

        let (kind, cmd) = BindTarget::Pane("list".into()).toml_fields("triage");
        assert_eq!(
            kind, "shell",
            "herdr has no keybinding type for opening a pane"
        );
        assert_eq!(
            cmd,
            "herdr plugin pane open --plugin triage --entrypoint list"
        );
    }

    #[test]
    fn a_bind_target_reports_its_id() {
        assert_eq!(BindTarget::Action("a".into()).id(), "a");
        assert_eq!(BindTarget::Pane("p".into()).id(), "p");
    }

    /// Refusing on a conflict is the whole safety story for writing to someone's herdr
    /// config: a second `[[keys.command]]` on the same key silently shadows the first, and
    /// the user would have no idea which binding they lost.
    #[test]
    fn existing_bindings_are_detected() {
        let config = r#"
onboarding = false

[[keys.command]]
key = "prefix+shift+l"
type = "plugin_action"
command = "herdr-lazy.manage"

# a commented-out one must not count
# key = "prefix+shift+z"

[[keys.command]]
key   =    "ctrl+alt+g"
command = "something.else"
"#;
        let keys = bound_keys(config);
        assert!(keys.contains(&"prefix+shift+l".to_string()));
        assert!(
            keys.contains(&"ctrl+alt+g".to_string()),
            "whitespace around = must not hide a binding"
        );
        assert!(
            !keys.contains(&"prefix+shift+z".to_string()),
            "a commented line is not a binding"
        );
    }

    #[test]
    fn an_empty_config_has_no_bindings() {
        assert!(bound_keys("").is_empty());
        assert!(bound_keys("onboarding = false\n[ui]\nx = 1\n").is_empty());
    }

    /// The config path comes from the socket herdr itself told us about, so a user with
    /// XDG_CONFIG_HOME pointed elsewhere does not get a second config file herdr never reads.
    #[test]
    fn config_path_sits_beside_the_socket() {
        // Safety: single-threaded test, and the variable is read immediately.
        unsafe { env::set_var("HERDR_SOCKET_PATH", "/somewhere/odd/herdr.sock") };
        assert_eq!(
            herdr_config_path(),
            Some(PathBuf::from("/somewhere/odd/config.toml"))
        );
        unsafe { env::remove_var("HERDR_SOCKET_PATH") };
        assert_eq!(herdr_config_path(), None, "no socket, no guessing");
    }

    #[test]
    fn spec_parses_ref_pin() {
        assert_eq!(
            Spec::parse("owner/repo@abc123"),
            Spec {
                repo: "owner/repo".into(),
                reference: Some("abc123".into())
            }
        );
        assert_eq!(Spec::parse("owner/repo").reference, None);
        assert_eq!(Spec::parse("owner/repo/sub").repo, "owner/repo/sub");
        assert_eq!(Spec::parse("owner/repo").display(), "owner/repo");
        assert_eq!(Spec::parse("owner/repo@v1").display(), "owner/repo@v1");
        // Degenerate forms must not silently produce an empty repo or empty --ref.
        assert_eq!(Spec::parse("owner/repo@").reference, None);
    }

    /// The bug the old substring matcher had: bundle `owner/herdr-lazy` counted an installed
    /// `herdr-lazy-extra` as satisfied, so the real plugin was never installed.
    #[test]
    fn prefix_names_do_not_match() {
        let extra = installed("herdr-lazy-extra", "github", &["owner/herdr-lazy-extra"]);
        assert_eq!(extra.matches(&Spec::parse("owner/herdr-lazy")), Match::None);
    }

    #[test]
    fn source_slug_is_a_strong_match() {
        let p = installed("anything", "github", &["github", "owner/repo"]);
        assert_eq!(p.matches(&Spec::parse("owner/repo")), Match::Strong);
        // A pin must not change identity — same repo, same plugin.
        assert_eq!(
            p.matches(&Spec::parse("owner/repo@deadbeef")),
            Match::Strong
        );
    }

    #[test]
    fn source_clone_urls_are_strong_matches() {
        for url in [
            "https://github.com/owner/repo",
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
        ] {
            assert_eq!(
                installed("x", "git", &[url]).matches(&Spec::parse("owner/repo")),
                Match::Strong,
                "{} should strongly match",
                url
            );
        }
        // A different owner shares the repo leaf but is NOT the same plugin.
        assert_eq!(
            installed("x", "git", &["https://github.com/other/repo"])
                .matches(&Spec::parse("owner/repo")),
            Match::None
        );
    }

    /// Name-only agreement is a guess: a manifest `name` need not equal the repo name.
    #[test]
    fn name_only_agreement_is_weak() {
        let p = installed("repo", "local", &["local"]);
        assert_eq!(p.matches(&Spec::parse("owner/repo")), Match::Weak);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let p = installed("X", "github", &["Owner/Repo"]);
        assert_eq!(p.matches(&Spec::parse("owner/repo")), Match::Strong);
    }

    #[test]
    fn unrelated_plugin_does_not_match() {
        let p = installed("something-else", "github", &["other/thing"]);
        assert_eq!(p.matches(&Spec::parse("owner/repo")), Match::None);
    }

    #[test]
    fn subdir_specs_match_their_source() {
        let p = installed("wm", "github", &["owner/repo/plugins/wm"]);
        assert_eq!(
            p.matches(&Spec::parse("owner/repo/plugins/wm")),
            Match::Strong
        );
    }
}
