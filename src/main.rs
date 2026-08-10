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
mod category;
mod extras;
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
/// And one that outranks both: somebody has to actually use it. Popularity is evidence of
/// that, not a substitute for it — `herdr-triage` was kept here on three stars after
/// `herdr-green` and `herdr-standup` were cut for being unused, which was the same mistake
/// with a better-looking number attached. It is gone for the same reason they were. The
/// author's own plugins clear this bar or leave, exactly as anyone else's do.
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
    // Listed so the tool can be updated by the tool. `u` in the pane, and `update` on the
    // command line, only act on entries in the list — leaving herdr-lazy out meant the one
    // plugin you could not update with herdr-lazy was herdr-lazy. `--prune` never removes it
    // either way; that is handled by identity, not by membership.
    //
    // Updating it replaces the binary of the running process. That is fine on Unix, where the
    // open inode outlives the rename, and it has been done on Windows too (see #2) — but it is
    // the reason this entry is worth a comment rather than being obvious.
    "natori-hrj/herdr-lazy",
    // Proven in the ecosystem, and verified to install cleanly.
    "cloudmanic/herdr-plus",                    // projects + quick actions
    "smarzban/herdr-file-viewer",               // git-aware read-only file pane
    "persiyanov/herdr-reviewr",                 // comment on an agent's diff, send it back
    "razajamil/herdr-plugin-workspace-manager", // per-workspace tab/pane layouts; no build step
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

/// The declared plugin list.
///
/// `HERDR_LAZY_LIST` moves it anywhere — the point being that a dotfiles user keeps it in
/// their repo and points here, instead of the file living buried in herdr's plugin config
/// dir mixed in with a cache they do not want to commit. Unset, it stays exactly where it
/// always was, so nothing changes for anyone not asking for this.
pub(crate) fn bundle_path() -> PathBuf {
    if let Ok(p) = env::var("HERDR_LAZY_LIST") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    config_dir().join("plugins.list")
}

/// The lock sits beside the list, wherever the list is.
///
/// It is generated, but it is also the file you copy to another machine to reproduce a
/// setup — the same reasoning that puts Cargo.lock next to Cargo.toml. So a dotfiles user who
/// moved their list into their repo gets the lock there too, both git-managed together.
fn lock_path() -> PathBuf {
    lock_beside(&bundle_path())
}

/// The lock that belongs next to a given list. Split out so the "beside the list" rule can be
/// tested without touching the environment the real path reads from.
fn lock_beside(list: &Path) -> PathBuf {
    list.with_file_name("plugins.lock")
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
    // When the location was chosen explicitly, do not auto-populate it. That path is likely a
    // dotfiles repo, and silently writing a legacy list into it is not ours to do — `init` or
    // `add` will, when the user asks.
    if env::var("HERDR_LAZY_LIST").is_ok_and(|v| !v.is_empty()) {
        return;
    }
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
                            .filter(|x| runs_here(x))
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
                            .filter(|x| runs_here(x))
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
fn cmd_probe(raw: bool) -> io::Result<()> {
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
    println!(
        "bundle         = {} {}",
        bundle_path().display(),
        file_note(&bundle_path())
    );
    println!(
        "lock           = {} {}",
        lock_path().display(),
        file_note(&lock_path())
    );
    println!();

    // 1. Can we reach the herdr binary at all?
    let version = match run_herdr(&["--version"]) {
        Ok((ok, out, err)) => {
            println!("[herdr --version] success={} {}", ok, out.trim());
            if !err.trim().is_empty() {
                println!("  stderr: {}", err.trim());
            }
            ok
        }
        Err(e) => {
            println!("[herdr --version] could not launch: {}", e);
            println!("\nVERDICT: cannot invoke herdr. Set HERDR_BIN_PATH or run inside herdr.");
            return Ok(());
        }
    };

    // 2. What does `plugin` actually expose?
    //
    // The verbatim help used to print here always. It earned that when the CLI surface was
    // unknown — grepping it for keywords had already hidden `list --json` and `install --ref`
    // once, and nearly cost a whole hand-rolled pinning layer. The surface is known now, so it
    // moves behind `--raw`: probe is the command someone runs to file a bug, and burying the
    // paths under a page of help text makes that report harder to write, not easier.
    let help = match run_herdr(&["plugin", "--help"]) {
        Ok((ok, out, err)) => {
            println!("[herdr plugin --help] success={}", ok);
            if raw {
                dump_block(&out, &err);
            }
            ok
        }
        Err(e) => {
            println!("[herdr plugin --help] could not run: {}", e);
            false
        }
    };

    // 3. The list format `sync` parses. `--json` is the contract.
    let list = match run_herdr(&["plugin", "list", "--json"]) {
        Ok((ok, out, err)) => {
            println!(
                "[herdr plugin list --json] success={} {}",
                ok,
                plugin_summary(&out)
            );
            if raw {
                dump_block(&out, &err);
            }
            ok
        }
        Err(e) => {
            println!("[herdr plugin list --json] could not run: {}", e);
            false
        }
    };

    if raw {
        match run_herdr(&["plugin", "list"]) {
            Ok((ok, out, err)) => {
                println!("[herdr plugin list] (human, for comparison) success={}", ok);
                dump_block(&out, &err);
            }
            Err(e) => println!("[herdr plugin list] could not run: {}", e),
        }
    }

    println!();
    if version && help && list {
        println!("VERDICT: the bridge works.");
    } else {
        println!("VERDICT: something above failed — the lines marked success=false are the ones to report.");
    }
    if !raw {
        println!("(`probe --raw` adds the full payloads, which is what a bug report may want.)");
    }
    Ok(())
}

/// `(4 entries)` / `(missing)` — enough to tell a working setup from an empty one at a glance.
fn file_note(p: &Path) -> String {
    if !p.exists() {
        return "(missing)".to_string();
    }
    format!("({} entries)", read_lines(p).len())
}

/// What `plugin list --json` amounts to, instead of the ~140 KB it takes to say it.
fn plugin_summary(stdout: &str) -> String {
    match parse_plugin_list(stdout) {
        Ok(ps) => {
            let github = ps.iter().filter(|p| p.slug.is_some()).count();
            format!("({} plugins, {} from github)", ps.len(), github)
        }
        Err(e) => format!("(could not be parsed: {})", e),
    }
}

/// Parse `--extras a,b` or `--extras=a,b` (repeatable) into a list of extra ids.
fn extras_arg(rest: &[&str]) -> Vec<String> {
    let split = |v: &str| {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if let Some(v) = rest[i].strip_prefix("--extras=") {
            out.extend(split(v));
        } else if rest[i] == "--extras" {
            if let Some(v) = rest.get(i + 1) {
                out.extend(split(v));
                i += 1;
            }
        }
        i += 1;
    }
    out
}

/// Parse `--from owner/repo` or `--from=owner/repo`.
fn from_arg<'a>(rest: &[&'a str]) -> Option<&'a str> {
    for (i, a) in rest.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--from=") {
            return (!v.is_empty()).then_some(v);
        }
        if *a == "--from" {
            return rest.get(i + 1).copied().filter(|v| !v.starts_with("--"));
        }
    }
    None
}

/// The list `init` writes: the curated defaults, then any chosen extras under their comments.
///
/// A function rather than inline, because the first-run bootstrap writes the same file. A new
/// machine and an explicit `init` must produce the same list, or "it worked on my laptop"
/// becomes a real answer.
fn default_bundle_body(chosen: &[extras::Extra]) -> String {
    let mut body = String::new();
    body.push_str("# herdr-lazy bundle — your declarative plugin set.\n");
    body.push_str("# One `owner/repo` per line. `#` starts a comment.\n");
    body.push_str("# Curated defaults below; edit to taste, then run `herdr-lazy sync`.\n\n");
    for d in DEFAULT_BUNDLE {
        body.push_str(d);
        body.push('\n');
    }

    // Each extra's plugins go under a comment naming it, skipping anything the defaults already
    // cover so a plugin is never listed twice.
    let mut seen: Vec<String> = DEFAULT_BUNDLE.iter().map(|s| s.to_string()).collect();
    for e in chosen {
        let fresh: Vec<&String> = e.plugins.iter().filter(|pl| !seen.contains(pl)).collect();
        if fresh.is_empty() {
            continue;
        }
        body.push('\n');
        body.push_str(&e.header());
        body.push('\n');
        for pl in fresh {
            body.push_str(pl);
            body.push('\n');
            seen.push(pl.clone());
        }
    }
    body
}

/// A line that declares a plugin, as opposed to a comment or a blank.
fn is_entry_line(l: &str) -> bool {
    let l = l.trim();
    !l.is_empty() && !l.starts_with('#')
}

/// `owner/repo`, optionally with a subdirectory — and nothing that looks like prose or markup.
///
/// This is the check that stops a fetched HTML error page, or a file that happens to live at
/// that path and is not a plugin list, from being written over someone's list.
fn looks_like_entry(l: &str) -> bool {
    let repo = Spec::parse(l).repo;
    !repo.is_empty()
        && repo.contains('/')
        && !repo.starts_with('/')
        && !repo.ends_with('/')
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c))
}

/// Fetch someone else's `plugins.list`: the file as written, and the entries in it.
///
/// Both callers need both halves — `init --from` writes the file verbatim so its comments
/// survive, while the pane shows one row per entry and lets you take some of them. Validating
/// once, here, is what stops a fetched HTML error page reaching either of them.
pub(crate) fn fetch_list(spec: &str) -> Result<(String, Vec<String>), String> {
    let s = Spec::parse(spec);
    if !looks_like_entry(&s.repo) || s.repo.matches('/').count() != 1 {
        return Err(format!(
            "`{}` is not an owner/repo (optionally owner/repo@ref)",
            spec
        ));
    }
    let body =
        github::raw_file(&s.repo, s.reference.as_deref(), "plugins.list").ok_or_else(|| {
            format!(
                "could not read plugins.list from {} — is there one at the repository root?",
                spec
            )
        })?;

    let entries: Vec<String> = body
        .lines()
        .filter(|l| is_entry_line(l))
        .map(|l| l.trim().to_string())
        .collect();
    if entries.is_empty() {
        return Err(format!("{} has a plugins.list, but it lists nothing", spec));
    }
    if let Some(bad) = entries.iter().find(|l| !looks_like_entry(l)) {
        return Err(format!(
            "{} does not look like a plugin list — found `{}`",
            spec, bad
        ));
    }
    Ok((body, entries))
}

/// Fetch someone else's `plugins.list` and return what to write.
///
/// A copy, deliberately: nothing records where it came from except a comment, there is no
/// upstream to track and no update channel. Adopting a list must not make the manager depend
/// on someone else's opinion — that is the whole difference between a starter and a
/// subscription.
///
/// The lockfile is not adopted with it. Taking someone's list means the same plugins; taking
/// their lock would mean the same commits, which is a far stronger claim to place in a
/// stranger's hands. If that is ever wanted it should be asked for separately.
fn adopt_body(spec: &str) -> Result<String, String> {
    let (body, _) = fetch_list(spec)?;

    // Provenance as a comment, not as state: it tells a reader where this came from without
    // creating anything the tool has to keep in step.
    let mut out = format!(
        "# adopted from {} — a copy, not a subscription. Edit it freely.\n\n",
        spec
    );
    out.push_str(&body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn cmd_init(
    force: bool,
    extra_ids: &[String],
    from: Option<&str>,
    dry_run: bool,
) -> io::Result<()> {
    let p = bundle_path();
    if p.exists() && !force {
        println!(
            "bundle already exists: {} (use `init --force` to overwrite)",
            p.display()
        );
        return Ok(());
    }

    // Resolve requested extras before writing anything, so an unknown id fails fast rather than
    // leaving a half-written bundle.
    let mut chosen = Vec::new();
    let mut unknown = Vec::new();
    for id in extra_ids {
        match extras::find(id) {
            Some(e) => chosen.push(e),
            None => unknown.push(id.as_str()),
        }
    }
    if !unknown.is_empty() {
        let known: Vec<String> = extras::all().iter().map(|e| e.id.clone()).collect();
        eprintln!("unknown extra(s): {}", unknown.join(", "));
        eprintln!("available: {} (see `herdr-lazy extras`)", known.join(", "));
        return Ok(());
    }

    // Someone else's list, if asked for — resolved before writing, so a repo that has no list
    // leaves the existing one alone rather than replacing it with the defaults.
    let adopted = match from {
        Some(spec) => match adopt_body(spec) {
            Ok(body) => Some((spec.to_string(), body)),
            Err(msg) => {
                eprintln!("{}", msg);
                return Ok(());
            }
        },
        None => None,
    };

    let base = match &adopted {
        Some((_, body)) => body.clone(),
        None => default_bundle_body(&chosen),
    };
    // Reading a list before it becomes your file is the point; this is the scriptable half.
    //
    // An adopted body is written verbatim and gets its extras from a second pass further down,
    // so they have to be printed here too. `default_bundle_body` already contains them, which
    // is why this only runs on the adopted path — printing both ways listed pluck twice.
    if dry_run {
        print!("{}", base);
        if adopted.is_some() {
            for e in &chosen {
                println!("\n{}", e.header());
                for pl in &e.plugins {
                    println!("{}", pl);
                }
            }
        }
        return Ok(());
    }
    ensure_parent(&p)?;
    fs::write(&p, base)?;
    // An adopted list is written verbatim, so any extras asked for still have to be appended —
    // the same append the pane uses, so the two cannot produce different files.
    if adopted.is_some() {
        for e in &chosen {
            let _ = add_extra_to_list(e);
        }
    }
    match &adopted {
        Some((spec, body)) => {
            let n = body.lines().filter(|l| is_entry_line(l)).count();
            println!("adopted {} plugin(s) from {} -> {}", n, spec, p.display());
            println!("it is your list now — no link back to {}.", spec);
        }
        None => println!("wrote curated default bundle -> {}", p.display()),
    }
    if !chosen.is_empty() {
        println!("with extras:");
        for e in &chosen {
            println!("  {} — {}", e.id, e.description);
        }
    }
    println!("edit it if you like, then run `herdr-lazy sync`.");
    Ok(())
}

/// List the opt-in extras, grouped by category.
fn cmd_extras() -> io::Result<()> {
    let all = extras::all();
    if all.is_empty() {
        println!("no extras are defined.");
        return Ok(());
    }
    println!("opt-in extras — add with `herdr-lazy init --extras <id,…>`:\n");
    // Categories in first-seen order; a plain Vec is enough for a handful of entries.
    let mut categories: Vec<String> = Vec::new();
    for e in &all {
        if !categories.iter().any(|c| c == &e.category) {
            categories.push(e.category.clone());
        }
    }
    for cat in &categories {
        println!("{}:", cat);
        for e in all.iter().filter(|e| &e.category == cat) {
            // Yours are marked. "Reviewed and shipped with the tool" and "a file I wrote last
            // Tuesday" are different claims, and the menu should not blur them.
            let mark = if e.source == extras::Source::Local {
                " (yours)"
            } else {
                ""
            };
            println!("  {:<12} {}{}", e.id, e.description, mark);
        }
        println!();
    }

    // Shadowing is allowed — it is your file — but never silent.
    let (local, problems) = extras::local();
    for e in &local {
        if extras::is_bundled(&e.id) {
            println!(
                "note: your `{}` replaces the bundled extra of that name.",
                e.id
            );
        }
    }
    for p in &problems {
        eprintln!("skipped {}", p);
    }
    if local.is_empty() && problems.is_empty() {
        println!(
            "write your own: drop `<id>.list` in {}",
            extras::local_dir().display()
        );
    }
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
    bootstrap_if_first_run();
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
    install_missing(&all, &installed);
    Ok(())
}

/// Install the entries that are absent, and refresh the lock.
///
/// Only what is absent. A drifted pin is a deliberate-looking state that `sync` repairs on
/// request; silently rewriting it at every launch would be a surprise.
///
/// Shared by startup auto-sync and the first-run bootstrap so the two cannot converge a
/// machine differently.
fn install_missing(all: &[Spec], installed: &[Installed]) -> usize {
    let pending = pending_work(all, installed);
    let missing: Vec<&Spec> = pending
        .iter()
        .filter(|spec| !installed.iter().any(|p| p.matches(spec) != Match::None))
        .collect();
    if missing.is_empty() {
        return 0;
    }

    println!(
        "herdr-lazy: installing {} plugin(s) declared in your list…",
        missing.len()
    );
    let mut done = 0;
    for spec in missing {
        let mut args = vec!["plugin", "install", spec.repo.as_str()];
        if let Some(r) = &spec.reference {
            args.push("--ref");
            args.push(r.as_str());
        }
        args.push("--yes");
        match run_herdr(&args) {
            Ok((true, _, _)) => {
                done += 1;
                println!("  installed {}", spec.display())
            }
            Ok((false, _, err)) => println!("  FAILED {}: {}", spec.display(), err.trim()),
            Err(e) => println!("  could not run herdr: {}", e),
        }
    }
    // Refresh the lock so it reflects what is now installed.
    if let Ok(after) = installed_plugins() {
        let _ = write_lock(all, &after);
    }
    done
}

/// The key the bootstrap binds. Shift, because herdr's own defaults live on `prefix+<letter>`
/// and are not exposed by the CLI — staying out of that range is the only way to be sure
/// nothing is shadowed. `l` for lazy; the README has documented this key since the beginning.
const BOOTSTRAP_KEY: &str = "prefix+shift+l";

/// Set a fresh machine up on the first herdr start after installing: write the list, install
/// what it names, and bind a key to the manage pane.
///
/// Without this, installing a plugin that calls itself a batteries-included distro leaves you
/// with an empty list and no way to open the pane — herdr has no command palette, and a
/// manifest cannot declare a keybinding, so nothing tells you the tool is there. Installing
/// herdr-lazy is the consent; this is what it consented to.
///
/// The opinion stays opt-in for everyone else, because this fires only on a machine that has
/// plainly never been set up: see `is_first_run`. It also never runs twice, and it never takes
/// a key that is already spoken for.
fn bootstrap_if_first_run() {
    if env::var("HERDR_LAZY_NO_BOOTSTRAP").is_ok_and(|v| !v.is_empty()) {
        return;
    }
    let marker = config_dir().join("bootstrapped");
    if marker.exists() {
        return;
    }
    // A list means this machine has been set up, whatever else is true. Record the decision so
    // the check below — which costs a round trip to herdr — never runs again.
    if bundle_path().exists() {
        let _ = ensure_parent(&marker);
        let _ = fs::write(&marker, DECLINED_MARKER);
        return;
    }
    let Ok(installed) = installed_plugins() else {
        return; // herdr not answering yet; try again next start
    };
    if !is_first_run(&installed) {
        let _ = ensure_parent(&marker);
        let _ = fs::write(&marker, DECLINED_MARKER);
        return;
    }

    println!("herdr-lazy: first run — setting up.");
    let p = bundle_path();
    if ensure_parent(&p).is_err() || fs::write(&p, default_bundle_body(&[])).is_err() {
        println!("  could not write {} — stopping here.", p.display());
        return;
    }
    println!("  wrote {}", p.display());

    let all: Vec<Spec> = desired_plugins().iter().map(|l| Spec::parse(l)).collect();
    install_missing(&all, &installed);

    // The action id is read back from herdr rather than hardcoded — see `platform_variant`.
    let action = installed
        .iter()
        .find(|p| is_self(p))
        .and_then(|me| platform_variant(me.actions.iter().map(|(id, _)| id.as_str()), "manage"));
    match action {
        Some(id) => match bind_action(PLUGIN_ID, &BindTarget::Action(id), BOOTSTRAP_KEY) {
            Ok(msg) => println!("  {}", msg),
            // A refusal here is the safe outcome, not a failure: the key was already taken, or
            // there is no config.toml to write. Say what to press instead of dying quietly.
            Err(msg) => {
                println!("  did not bind {}: {}", BOOTSTRAP_KEY, msg);
                println!("  open the pane with: {}", manage_pane_command(&installed));
            }
        },
        None => println!(
            "  no manage action registered for this platform — open the pane with: {}",
            manage_pane_command(&installed)
        ),
    }

    let _ = ensure_parent(&marker);
    let _ = fs::write(&marker, DONE_MARKER);
    println!("  done — press {} to manage your plugins.", BOOTSTRAP_KEY);
}

/// What herdr calls the platform we are running on, for comparing against a manifest's
/// `platforms`. herdr's own vocabulary, not Rust's — `macos`, not `darwin`.
fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Can this action or pane run on this machine?
///
/// `plugin list --json` reports every entry a plugin declares, including the ones gated to
/// other platforms — herdr filters at *invocation* (`platform_unsupported`), not in the
/// listing. Since a plugin that supports Windows has to declare separate entries with their
/// own ids, an unfiltered listing shows the same action twice under the same title, half of
/// which refuse to run. Filtering here means every reader of `Installed` — the details view,
/// the bind menu, the first-run bootstrap — sees only what this machine can actually do.
///
/// No `platforms` means every platform, which is how most manifests are written.
fn runs_here(entry: &json::Value) -> bool {
    match entry.get("platforms").and_then(|p| p.as_array()) {
        Some(ps) if !ps.is_empty() => ps
            .iter()
            .filter_map(|p| p.as_str())
            .any(|p| p == current_platform()),
        _ => true,
    }
}

/// The id herdr registered for one of our own entries on *this* platform.
///
/// Not a constant, because it is not the same everywhere. herdr rejects duplicate action and
/// pane ids even when the entries are gated to platforms that cannot overlap, so a platform
/// needing a differently-shaped command needs a differently-named entry too — `manage` on
/// Unix, `manage-windows` on Windows. Binding the wrong one produces a key that reports
/// success and does nothing, since the refusal (`platform_unsupported`) happens later, at a
/// keypress, where nobody sees it.
///
/// The candidates come from `Installed`, which `runs_here` has already narrowed to this
/// platform — so this only has to tolerate the naming, not decide the platform. An exact match
/// wins; otherwise the first `<base>-<suffix>` variant.
fn platform_variant<'a>(ids: impl Iterator<Item = &'a str>, base: &str) -> Option<String> {
    let mut variant = None;
    for id in ids {
        if id == base {
            return Some(id.to_string());
        }
        if id
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('-'))
            && variant.is_none()
        {
            variant = Some(id.to_string());
        }
    }
    variant
}

/// The command that opens our manage pane by hand, for a message telling someone to run it.
fn manage_pane_command(installed: &[Installed]) -> String {
    let id = installed
        .iter()
        .find(|p| is_self(p))
        .and_then(|me| platform_variant(me.panes.iter().map(|(id, _, _)| id.as_str()), "manage"))
        .unwrap_or_else(|| "manage".to_string());
    format!(
        "herdr plugin pane open --plugin {} --entrypoint {} --focus",
        PLUGIN_ID, id
    )
}

/// The same, for a caller that has not already asked herdr what is installed.
pub(crate) fn manage_pane_hint() -> String {
    manage_pane_command(&installed_plugins().unwrap_or_default())
}

const DONE_MARKER: &str = "herdr-lazy set this machine up once; delete this file to redo it\n";
const DECLINED_MARKER: &str = "this machine was already set up, so herdr-lazy left it alone\n";

/// Is this a machine herdr-lazy has never touched?
///
/// Only when the sole installed plugin is herdr-lazy itself. Someone with plugins but no list
/// built their setup by hand and is a candidate for adopting it (`a` in the pane) — pushing
/// five more plugins at them uninvited is exactly the imposition this whole design avoids.
fn is_first_run(installed: &[Installed]) -> bool {
    installed.iter().all(is_self)
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

/// Which of an extra's plugins a list does not already declare.
///
/// Compared by repo rather than by line, so an entry the user has pinned (`owner/repo@v1`)
/// counts as present — re-adding it unpinned would quietly undo their pin on the next sync.
fn fresh_plugins(e: &extras::Extra, existing: &[String]) -> Vec<String> {
    let listed: Vec<String> = existing.iter().map(|l| Spec::parse(l).repo).collect();
    e.plugins
        .iter()
        .filter(|pl| !listed.iter().any(|l| l == *pl))
        .cloned()
        .collect()
}

/// Append an extra's plugins to the list, under the comment naming it. Returns what was added,
/// which is empty when the list already covers the whole extra.
///
/// Skipping what is already listed is what makes this safe to press twice: the same extra
/// applied again is a no-op rather than a second copy of its entries. The header is only
/// written when there is something to write under it, so a repeat leaves no orphan comment.
pub(crate) fn add_extra_to_list(e: &extras::Extra) -> io::Result<Vec<String>> {
    add_extra_at(&bundle_path(), e)
}

/// The whole of the above, against a given file — so the write can be tested against a real
/// one without an environment variable that other tests would race on.
fn add_extra_at(p: &Path, e: &extras::Extra) -> io::Result<Vec<String>> {
    let fresh = fresh_plugins(e, &read_lines(p));
    if fresh.is_empty() {
        return Ok(fresh);
    }
    ensure_parent(p)?;
    let mut body = fs::read_to_string(p).unwrap_or_default();
    // A blank line separates the block from what is above it — but only when there is
    // something above it, so a list created by this does not start with an empty line.
    if !body.is_empty() {
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push('\n');
    }
    body.push_str(&e.header());
    body.push('\n');
    for pl in &fresh {
        body.push_str(pl);
        body.push('\n');
    }
    fs::write(p, body)?;
    Ok(fresh)
}

/// Append entries taken from someone else's list, under one comment naming where they came
/// from. Returns what was added, which is empty when your list already covers all of them.
///
/// One comment above the block rather than one per line: the provenance is a fact about the
/// act of taking them, not about each plugin, and a list is meant to stay readable.
pub(crate) fn add_adopted_to_list(spec: &str, entries: &[String]) -> io::Result<Vec<String>> {
    let p = bundle_path();
    let listed: Vec<String> = read_lines(&p).iter().map(|l| Spec::parse(l).repo).collect();
    let fresh: Vec<String> = entries
        .iter()
        .filter(|e| !listed.contains(&Spec::parse(e).repo))
        .cloned()
        .collect();
    if fresh.is_empty() {
        return Ok(fresh);
    }
    ensure_parent(&p)?;
    let mut body = fs::read_to_string(&p).unwrap_or_default();
    if !body.is_empty() {
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push('\n');
    }
    body.push_str(&format!("# from {}\n", spec));
    for e in &fresh {
        body.push_str(e);
        body.push('\n');
    }
    fs::write(&p, body)?;
    Ok(fresh)
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
    println!("  probe [--raw]     verify the plugin <-> herdr CLI bridge (run this first)");
    println!("  init [--force] [--extras <id,…>] [--from <owner/repo[@ref]>] [--dry-run]");
    println!("                    write the default bundle, or adopt someone else's list");
    println!("  extras            list the opt-in extras you can pass to `init --extras`");
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
        "probe" => cmd_probe(rest.contains(&"--raw") || rest.contains(&"--verbose")),
        "startup" => cmd_startup(),
        "auto-sync" => cmd_auto_sync(rest.first().copied()),
        "init" => cmd_init(
            rest.contains(&"--force"),
            &extras_arg(&rest),
            from_arg(&rest),
            rest.contains(&"--dry-run"),
        ),
        "extras" => cmd_extras(),
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
    fn the_lock_always_sits_beside_the_list() {
        // Wherever the list is moved, the lock follows into the same directory, so a dotfiles
        // user gets both in their repo rather than the lock stranded in herdr's config dir.
        assert_eq!(
            lock_beside(Path::new("/home/me/dotfiles/herdr/plugins.list")),
            Path::new("/home/me/dotfiles/herdr/plugins.lock")
        );
        assert_eq!(
            lock_beside(Path::new("plugins.list")),
            Path::new("plugins.lock")
        );
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

    /// The bootstrap fires only on a machine that has plainly never been set up. Getting this
    /// wrong installs five plugins on someone who did not ask — the one thing it must not do.
    #[test]
    fn only_a_machine_with_nothing_but_herdr_lazy_is_a_first_run() {
        let me = installed("herdr-lazy", "github", &["natori-hrj/herdr-lazy"]);
        let me = Installed {
            plugin_id: "herdr-lazy".to_string(),
            ..me
        };
        assert!(is_first_run(&[]), "a herdr with no plugins at all");
        assert!(is_first_run(std::slice::from_ref(&me)), "only herdr-lazy");
        assert!(
            !is_first_run(&[me, from_github("cloudmanic", "herdr-plus")]),
            "a hand-built setup must be left alone"
        );
    }

    /// Verbatim shape of a plugin that supports Windows: herdr lists every declared entry,
    /// including the ones gated elsewhere, and each carries its own `platforms`.
    const WINDOWS_TWINS: &str = r#"{"id":"cli:plugin","result":{"plugins":[{"plugin_id":"herdr-lazy","name":"herdr-lazy","enabled":true,"source":{"kind":"github","owner":"natori-hrj","repo":"herdr-lazy"},"actions":[{"id":"probe","title":"Lazy: probe CLI bridge","platforms":["linux","macos"],"command":["./target/release/herdr-lazy","probe"]},{"id":"probe-windows","title":"Lazy: probe CLI bridge","platforms":["windows"],"command":["powershell","-Command","probe"]},{"id":"everywhere","title":"No platforms declared"}],"panes":[{"id":"manage","title":"herdr-lazy","placement":"overlay","platforms":["linux","macos"]},{"id":"manage-windows","title":"herdr-lazy","placement":"overlay","platforms":["windows"]}]}],"type":"plugin_list"}}"#;

    /// The listing is not filtered by herdr, so it is filtered here. Without this, a plugin
    /// that supports Windows shows every action twice under the same title on macOS, and half
    /// of them refuse to run (`platform_unsupported`).
    #[test]
    fn entries_for_other_platforms_are_not_shown() {
        let ps = parse_plugin_list(WINDOWS_TWINS).expect("payload should parse");
        let ids: Vec<&str> = ps[0].actions.iter().map(|(id, _)| id.as_str()).collect();
        let panes: Vec<&str> = ps[0].panes.iter().map(|(id, _, _)| id.as_str()).collect();

        // An entry declaring no platforms runs everywhere, which is how most manifests read.
        assert!(ids.contains(&"everywhere"));
        if cfg!(target_os = "windows") {
            assert_eq!(ids, vec!["probe-windows", "everywhere"]);
            assert_eq!(panes, vec!["manage-windows"]);
        } else {
            assert_eq!(ids, vec!["probe", "everywhere"]);
            assert_eq!(panes, vec!["manage"]);
        }
    }

    /// The listing is filtered before this sees it, so exactly one `manage` survives — but its
    /// id still differs by platform, which is what this resolves.
    #[test]
    fn an_ids_platform_variant_is_found_and_an_exact_match_wins() {
        assert_eq!(
            platform_variant(["probe", "init", "manage"].into_iter(), "manage"),
            Some("manage".to_string())
        );
        assert_eq!(
            platform_variant(["probe-windows", "manage-windows"].into_iter(), "manage"),
            Some("manage-windows".to_string())
        );
        // An exact match wins wherever it sits, so a platform registering both is not a toss-up.
        assert_eq!(
            platform_variant(["manage-windows", "manage"].into_iter(), "manage"),
            Some("manage".to_string())
        );
        // A name that merely starts with the base is not a variant of it.
        assert_eq!(platform_variant(["managed"].into_iter(), "manage"), None);
        assert_eq!(platform_variant([].into_iter(), "manage"), None);
    }

    /// `init` and the first-run bootstrap write the same file, so a new machine and an explicit
    /// init cannot disagree about what the curated set is.
    #[test]
    fn the_bootstrap_writes_the_same_list_init_does() {
        let body = default_bundle_body(&[]);
        for d in DEFAULT_BUNDLE {
            assert!(body.contains(d), "{} missing from the default list", d);
        }
        assert!(!body.contains("# extra:"), "no extras unless asked for");
        assert_eq!(
            read_lines(Path::new("/nonexistent")).len(),
            0,
            "sanity: a missing list reads as empty"
        );
    }

    #[test]
    fn chosen_extras_are_appended_under_their_own_comment() {
        let body = default_bundle_body(&[extra(&["owner/new"])]);
        assert!(body.contains("# extra: worktrunk — switch worktrees from a picker"));
        assert!(body.contains("owner/new"));
    }

    fn extra(plugins: &[&str]) -> extras::Extra {
        extras::Extra {
            id: "worktrunk".to_string(),
            source: extras::Source::Bundled,
            category: "worktree".to_string(),
            description: "switch worktrees from a picker".to_string(),
            plugins: plugins.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn an_extra_only_adds_what_the_list_does_not_have() {
        let existing = vec!["owner/have".to_string()];
        assert_eq!(
            fresh_plugins(&extra(&["owner/have", "owner/new"]), &existing),
            vec!["owner/new".to_string()]
        );
    }

    /// Applying an extra twice must be a no-op, not a second copy of its entries.
    #[test]
    fn an_extra_already_listed_adds_nothing() {
        let existing = vec!["owner/have".to_string()];
        assert!(fresh_plugins(&extra(&["owner/have"]), &existing).is_empty());
    }

    /// A pinned entry is the same plugin. Re-adding it unpinned would undo the pin, which is
    /// the one way this feature could take something away from a user rather than add to it.
    #[test]
    fn a_pinned_entry_counts_as_already_listed() {
        let existing = vec!["owner/have@v1.2.0".to_string()];
        assert!(fresh_plugins(&extra(&["owner/have"]), &existing).is_empty());
    }

    /// A path of our own in the temp directory, named after the test so two never collide.
    fn scratch_list(name: &str) -> PathBuf {
        let p = env::temp_dir().join(format!("herdr-lazy-{}-{}.list", std::process::id(), name));
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn applying_an_extra_appends_it_under_a_comment_naming_it() {
        let p = scratch_list("append");
        fs::write(&p, "owner/already\n").unwrap();
        let added = add_extra_at(&p, &extra(&["owner/already", "owner/new"])).unwrap();
        assert_eq!(added, vec!["owner/new".to_string()]);
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "owner/already\n\n# extra: worktrunk — switch worktrees from a picker\nowner/new\n"
        );
        let _ = fs::remove_file(&p);
    }

    /// The picker cannot stop someone pressing enter twice, so the write has to be idempotent —
    /// no duplicate entry, and no orphan comment with nothing under it.
    #[test]
    fn applying_the_same_extra_twice_changes_nothing_the_second_time() {
        let p = scratch_list("twice");
        let e = extra(&["owner/new"]);
        add_extra_at(&p, &e).unwrap();
        let after_first = fs::read_to_string(&p).unwrap();
        assert!(add_extra_at(&p, &e).unwrap().is_empty());
        assert_eq!(fs::read_to_string(&p).unwrap(), after_first);
        let _ = fs::remove_file(&p);
    }

    /// Applied to a list that does not exist yet, the file must not open with a blank line.
    #[test]
    fn a_list_created_by_an_extra_starts_with_the_comment() {
        let p = scratch_list("fresh");
        add_extra_at(&p, &extra(&["owner/new"])).unwrap();
        assert!(fs::read_to_string(&p).unwrap().starts_with("# extra: "));
        let _ = fs::remove_file(&p);
    }

    /// Both writers of the list use this one line, so it is worth pinning down.
    #[test]
    fn the_extra_comment_names_the_extra_and_what_it_is_for() {
        assert_eq!(
            extra(&["owner/repo"]).header(),
            "# extra: worktrunk — switch worktrees from a picker"
        );
    }

    /// Returns `(table, platforms, program)` for every entry in the manifest, where
    /// `program` is the first element of `command` and `platforms` is empty when the
    /// entry declares none.
    ///
    /// Hand-rolled rather than pulling in a TOML parser: the shapes here are flat
    /// single-line arrays, and the project keeps its dependency list at one.
    struct ManifestEntry {
        table: String,
        id: String,
        platforms: Vec<String>,
        program: String,
    }

    impl ManifestEntry {
        fn declares(&self, platform: &str) -> bool {
            self.platforms.iter().any(|p| p == platform)
        }
        /// No `platforms` means every platform, which is how most entries are written.
        fn everywhere(&self) -> bool {
            self.platforms.is_empty()
        }
    }

    fn manifest_entries(src: &str) -> Vec<ManifestEntry> {
        fn array_items(line: &str) -> Vec<String> {
            let mut out = Vec::new();
            let rest = match line.split_once('[') {
                Some((_, r)) => r,
                None => return out,
            };
            let mut chars = rest.chars();
            let mut cur = String::new();
            let mut inside = false;
            for c in chars.by_ref() {
                match c {
                    '"' if inside => {
                        out.push(std::mem::take(&mut cur));
                        inside = false;
                    }
                    '"' => inside = true,
                    ']' if !inside => break,
                    _ if inside => cur.push(c),
                    _ => {}
                }
            }
            out
        }

        /// `id = "probe"` -> `probe`
        fn quoted_value(line: &str) -> String {
            line.split('"').nth(1).unwrap_or_default().to_string()
        }

        let mut entries: Vec<ManifestEntry> = Vec::new();
        for raw in src.lines() {
            let line = raw.trim();
            if line.starts_with('#') {
                continue;
            }
            if line.starts_with("[[") {
                entries.push(ManifestEntry {
                    table: line.trim_matches(|c| c == '[' || c == ']').to_string(),
                    id: String::new(),
                    platforms: Vec::new(),
                    program: String::new(),
                });
                continue;
            }
            let Some(last) = entries.last_mut() else {
                continue;
            };
            if line.starts_with("platforms") {
                last.platforms = array_items(line);
            } else if line.starts_with("command") {
                last.program = array_items(line).first().cloned().unwrap_or_default();
            } else if line.starts_with("id") {
                last.id = quoted_value(line);
            }
        }
        entries
    }

    /// herdr hands a plugin's command to the platform's own process spawner. On Windows
    /// that is CreateProcessW, which neither appends `.exe` nor resolves a relative
    /// program the way a shell would, so `./target/release/herdr-lazy` never launches —
    /// the action fails with no exit code and no stderr, which is a miserable thing to
    /// debug. `/bin/sh` is not there to run either.
    ///
    /// An entry that declares no `platforms` applies to every platform, so it counts as
    /// Windows-reachable. That is precisely how this was wrong: the actions, the startup
    /// command and the pane were all unqualified, and all four were unspawnable.
    #[test]
    fn every_windows_reachable_command_names_a_program_windows_can_spawn() {
        for e in manifest_entries(include_str!("../herdr-plugin.toml")) {
            if !(e.everywhere() || e.declares("windows")) || e.program.is_empty() {
                continue;
            }
            let (table, program) = (&e.table, &e.program);
            assert!(
                !program.starts_with("./"),
                "[[{table}]] command `{program}` is relative; CreateProcessW will not resolve it"
            );
            assert!(
                !program.starts_with('/'),
                "[[{table}]] command `{program}` is a POSIX absolute path that Windows cannot run"
            );
        }
    }

    #[test]
    fn from_takes_a_repo_in_either_form() {
        assert_eq!(from_arg(&["--from", "owner/repo"]), Some("owner/repo"));
        assert_eq!(from_arg(&["--from=owner/repo@v1"]), Some("owner/repo@v1"));
        assert_eq!(from_arg(&["init", "--force"]), None);
        // A flag is not a value: `--from --force` must not adopt a repo called "--force".
        assert_eq!(from_arg(&["--from", "--force"]), None);
        assert_eq!(from_arg(&["--from"]), None);
    }

    /// The guard that stops a fetched HTML error page — or any file that is not a plugin list —
    /// being written over someone's list.
    #[test]
    fn only_owner_repo_lines_count_as_entries() {
        assert!(looks_like_entry("owner/repo"));
        assert!(looks_like_entry("owner/repo@v1.2.0"));
        assert!(looks_like_entry("owner/repo/plugins/sub"));
        assert!(!looks_like_entry("<!DOCTYPE html>"));
        assert!(!looks_like_entry("404: Not Found"));
        assert!(!looks_like_entry("just-a-name"));
        assert!(!looks_like_entry("/leading"));
        assert!(!looks_like_entry("trailing/"));
        assert!(!looks_like_entry(""));
    }

    #[test]
    fn comments_and_blanks_are_not_entries() {
        assert!(is_entry_line("owner/repo"));
        assert!(!is_entry_line("# a comment"));
        assert!(!is_entry_line("   "));
    }

    /// probe is the first thing someone runs when filing a bug, so what it says about the
    /// payload has to be right — the count is the whole reason not to print the payload.
    #[test]
    fn probe_summarises_the_plugin_list_instead_of_printing_it() {
        assert_eq!(plugin_summary(LINKED_LOCAL), "(1 plugins, 0 from github)");
        assert!(plugin_summary("not json at all").starts_with("(could not be parsed"));
    }

    #[test]
    fn probe_says_whether_a_file_is_there_and_how_big() {
        let p = scratch_list("probe");
        assert_eq!(file_note(&p), "(missing)");
        fs::write(&p, "# a comment\nowner/one\nowner/two\n").unwrap();
        assert_eq!(file_note(&p), "(2 entries)", "comments are not entries");
        let _ = fs::remove_file(&p);
    }

    /// The floor herdr enforces and the floor the README promises have to be the same number.
    ///
    /// They disagreed once already — the manifest said 0.7.0 while the first-run setup needed
    /// the 0.7.5 startup hook — and the failure mode was silent: herdr-lazy installed on an
    /// older herdr and then did nothing, with no way to say why, because the missing hook is
    /// the thing that would have printed the message.
    #[test]
    fn the_readme_and_the_manifest_agree_on_the_herdr_floor() {
        let floor = include_str!("../herdr-plugin.toml")
            .lines()
            .find(|l| l.trim_start().starts_with("min_herdr_version"))
            .and_then(|l| l.split('"').nth(1))
            .expect("the manifest declares a floor");
        let promised = format!("Requires herdr ≥ {}", floor);
        assert!(
            include_str!("../README.md").contains(&promised),
            "README does not say `{}`",
            promised
        );
    }

    /// Until herdr resolves relative commands on Windows (#28), every action and pane is
    /// declared twice — once for Unix, once for Windows under a `-windows` id. Nothing stops
    /// someone adding only one half, and nothing would fail: the test above only checks that
    /// what *is* declared can be spawned, so a feature silently missing on one platform is
    /// green CI.
    ///
    /// This is the guard that makes carrying the split entries safe rather than merely ugly.
    /// It goes away with them.
    #[test]
    fn platform_split_entries_come_in_pairs() {
        const SUFFIX: &str = "-windows";
        let entries = manifest_entries(include_str!("../herdr-plugin.toml"));

        for table in ["actions", "panes"] {
            let of_table = || entries.iter().filter(|e| e.table == table);
            let windows: Vec<&str> = of_table()
                .filter(|e| e.declares("windows"))
                .map(|e| e.id.as_str())
                .collect();
            let unix: Vec<&str> = of_table()
                .filter(|e| !e.everywhere() && !e.declares("windows"))
                .map(|e| e.id.as_str())
                .collect();

            for id in &unix {
                let twin = format!("{id}{SUFFIX}");
                assert!(
                    windows.contains(&twin.as_str()),
                    "[[{table}]] `{id}` has no `{twin}` — it would be missing on Windows"
                );
            }
            for id in &windows {
                let stem = id.strip_suffix(SUFFIX).unwrap_or(id);
                assert!(
                    unix.contains(&stem),
                    "[[{table}]] `{id}` has no `{stem}` — it would be missing on Unix"
                );
            }
        }

        // `[[startup]]` has no id to pair on, so the check is coverage: every platform this
        // plugin claims to support must have a startup hook, or the first-run bootstrap and
        // auto-sync simply never fire there.
        let startup = || entries.iter().filter(|e| e.table == "startup");
        for platform in ["linux", "macos", "windows"] {
            assert!(
                startup().any(|e| e.everywhere() || e.declares(platform)),
                "no [[startup]] entry runs on {platform}"
            );
        }
    }
}
