//! Read-only evidence for Plugin Details. Do not reuse operational matching or pin enforcement
//! here: they deliberately tolerate evidence that does not establish provenance.

use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use crate::{registry, Installed};

const UNKNOWN: &str = "Unknown";
const MAX_IDENTITY_INPUT: usize = 320;
const MAX_LOCK_BYTES: u64 = 256 * 1024;
const BUNDLE: &str = "bundle list";
const REGISTRY: &str = "installed registry";
const LOCK: &str = "plugins.lock";

/// Read only on opening Details, never during startup or a redraw. Don't interpret a truncated,
/// invalid or unreadable snapshot: a later record could make an apparent match ambiguous.
pub(crate) fn read_lock(path: &Path) -> Option<String> {
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_LOCK_BYTES {
        return None;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A path replacement must not block Details on a named pipe.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_LOCK_BYTES {
        return None;
    }
    read_lock_contents(file)
}

fn read_lock_contents(reader: impl Read) -> Option<String> {
    String::from_utf8(crate::read_limited(reader, MAX_LOCK_BYTES).ok()??).ok()
}

fn strip_alias(value: &str) -> Option<&str> {
    [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|prefix| {
        let (head, tail) = (value.get(..prefix.len())?, value.get(prefix.len()..)?);
        head.eq_ignore_ascii_case(prefix).then_some(tail)
    })
}

fn alias_slug(path: &str) -> Option<&str> {
    // A clone URL can establish the repository root, not a plugin subdirectory. In particular,
    // don't interpret GitHub web paths such as /tree/main/... as a bundle entry.
    let (owner, repo) = path.split_once('/')?;
    if repo.contains('/') || owner.is_empty() || repo.is_empty() {
        return None;
    }
    if path
        .as_bytes()
        .get(path.len().saturating_sub(4)..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(b".git"))
    {
        Some(&path[..path.len() - 4])
    } else {
        Some(path)
    }
}

fn repository(value: &str) -> Option<String> {
    if value.len() > MAX_IDENTITY_INPUT {
        return None;
    }
    let slug = match strip_alias(value) {
        Some(path) => alias_slug(path)?,
        None => value,
    };
    if slug.len() > 255
        || slug.starts_with('-')
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
        || slug.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        return None;
    }
    let (owner, rest) = slug.split_once('/')?;
    let (repo, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut normalized = format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    );
    if !path.is_empty() {
        normalized.push('/');
        normalized.push_str(path);
    }
    Some(normalized)
}

fn safe_ref(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.ends_with('.')
        && value != "@"
        && !value.contains("@{")
        && !value.contains("..")
        && !value.contains("//")
        && !value
            .split('/')
            .any(|component| component.starts_with('.') || component.ends_with(".lock"))
        && value
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"~^:?*[\\".contains(&b)))
    .then_some(value)
}

fn full_digest(value: &str) -> Option<&str> {
    (matches!(value.len(), 40 | 64) && crate::is_commit_ref(value)).then_some(value)
}

enum RegistryIdentity {
    Known(String, &'static str),
    Conflict {
        structured: String,
        alias: Option<String>,
    },
    Unidentified,
}

#[derive(Debug, Clone, Default)]
pub(crate) enum SourceUrl {
    #[default]
    Absent,
    Invalid,
    GitHubRoot(String),
}

impl SourceUrl {
    pub(crate) fn from_field(field: Option<&crate::json::Value>) -> Self {
        match field {
            None => Self::Absent,
            Some(value) => value
                .as_str()
                .filter(|url| strip_alias(url).is_some())
                .and_then(repository)
                .map_or(Self::Invalid, Self::GitHubRoot),
        }
    }

    fn alias(&self) -> Option<&str> {
        match self {
            Self::GitHubRoot(root) => Some(root),
            Self::Absent | Self::Invalid => None,
        }
    }
}

fn registry_identity(plugin: &Installed) -> RegistryIdentity {
    if plugin.source_kind == "local" {
        return RegistryIdentity::Unidentified;
    }
    let structured = if plugin.source_kind == "github" {
        plugin.slug.as_deref().and_then(repository)
    } else {
        None
    };
    let alias = plugin.source_url.alias();
    match (structured, alias) {
        (Some(structured), alias)
            if plugin.source_owner_has_path
                || (!matches!(plugin.source_url, SourceUrl::Absent)
                    && alias != Some(crate::repo_root(&structured).as_str())) =>
        {
            RegistryIdentity::Conflict {
                structured,
                alias: alias.map(str::to_owned),
            }
        }
        (Some(slug), _) => RegistryIdentity::Known(slug, REGISTRY),
        (None, Some(slug)) => RegistryIdentity::Known(slug.to_owned(), "installed registry URL"),
        (None, None) => RegistryIdentity::Unidentified,
    }
}

// Dispatch uses the registry ID, not the display name or repository. A conflicting ID anywhere
// in the snapshot makes even an otherwise exact scope unsafe to address.
fn unique_id(plugin: &Installed, installed: &[Installed]) -> bool {
    let id = &plugin.plugin_id;
    !id.is_empty()
        && !id.starts_with('-')
        && !matches!(id.as_str(), "." | "..")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !installed
            .iter()
            .any(|other| !std::ptr::eq(other, plugin) && other.plugin_id == *id)
}

fn entry_record(line: &str) -> Option<(String, Option<&str>)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    // Split after the host prefix so the '@' in an SSH transport isn't treated as a ref.
    let path = strip_alias(line).unwrap_or(line);
    let prefix_len = line.len() - path.len();
    let (repo, reference) = match path.find('@') {
        Some(index) => {
            let (left, right) = line.split_at(prefix_len + index);
            (left.trim_end(), safe_ref(right[1..].trim()))
        }
        None => (line, None),
    };
    Some((repository(repo)?, reference))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Report {
    repository: Option<(String, &'static str)>,
    requested: Option<String>,
    resolved: Option<String>,
    source: Option<&'static str>,
    installed_ms: Option<u64>,
    last_push: Option<String>,
    lock_ref: Option<String>,
    consistency: Option<&'static str>,
}

impl Report {
    /// Snapshot facts and, separately, the installed record authorized to supply a UI payload.
    /// A unique registry fact need not be the record the operational matcher chose.
    pub(crate) fn for_bundle<'a>(
        displayed_spec: &str,
        installed: &'a [Installed],
        operational_hit: Option<&Installed>,
        market: &[registry::Entry],
    ) -> (Self, Option<&'a Installed>) {
        // Reparse the original display for evidence only: Spec::parse may have split at the
        // transport '@' in an SSH alias instead of the requested ref.
        let (repository, requested) = match entry_record(displayed_spec) {
            Some((slug, requested)) => (Some((slug, BUNDLE)), requested),
            None => (None, None),
        };
        let wanted = repository.as_ref().map(|(slug, _)| slug.as_str());
        let root = wanted.map(crate::repo_root);
        let mut evidence = None;
        let mut ambiguous_or_conflicting = false;
        for plugin in installed {
            match registry_identity(plugin) {
                RegistryIdentity::Known(slug, _) => {
                    if wanted == Some(slug.as_str()) {
                        ambiguous_or_conflicting |= evidence.replace(plugin).is_some();
                    } else if root.as_deref() == Some(slug.as_str()) {
                        // For a child, a root-only record may be describing that same child
                        // without its path. It cannot prove identity or break an apparent tie.
                        ambiguous_or_conflicting = true;
                    }
                }
                RegistryIdentity::Conflict { structured, alias } => {
                    ambiguous_or_conflicting |= wanted == Some(structured.as_str())
                        || root.as_deref() == Some(structured.as_str())
                        || root
                            .as_ref()
                            .is_some_and(|root| alias.as_ref() == Some(root));
                }
                RegistryIdentity::Unidentified => {}
            }
        }
        let evidence = if ambiguous_or_conflicting {
            None
        } else {
            evidence
        };
        let authorized = evidence.filter(|plugin| {
            operational_hit.is_some_and(|hit| std::ptr::eq(hit, *plugin))
                && unique_id(plugin, installed)
        });
        (
            Self::from_evidence(repository, requested, evidence, market),
            authorized,
        )
    }

    pub(crate) fn for_install<'a>(
        plugin: &'a Installed,
        installed: &[Installed],
        market: &[registry::Entry],
    ) -> (Self, Option<&'a Installed>) {
        let identity = registry_identity(plugin);
        let authorized = (!matches!(identity, RegistryIdentity::Conflict { .. })
            && unique_id(plugin, installed))
        .then_some(plugin);
        let report = match identity {
            RegistryIdentity::Known(slug, source) => {
                Self::from_evidence(Some((slug, source)), None, Some(plugin), market)
            }
            RegistryIdentity::Unidentified => Self::from_evidence(None, None, Some(plugin), market),
            RegistryIdentity::Conflict { .. } => Self::default(),
        };
        (report, authorized)
    }

    fn from_evidence(
        repository: Option<(String, &'static str)>,
        requested: Option<&str>,
        installed: Option<&Installed>,
        market: &[registry::Entry],
    ) -> Self {
        let last_push = repository
            .as_ref()
            .and_then(|(slug, _)| cached_last_push(&crate::repo_root(slug), market));
        let source = installed.and_then(|plugin| match plugin.source_kind.as_str() {
            "github" => Some("GitHub"),
            "local" => Some("Local link"),
            _ if repository.is_some() => Some("GitHub URL"),
            _ => None,
        });
        Self {
            repository,
            requested: requested.and_then(safe_ref).map(str::to_owned),
            resolved: installed
                .and_then(|plugin| plugin.resolved_commit.as_deref())
                .and_then(full_digest)
                .map(str::to_owned),
            source,
            installed_ms: installed.and_then(|plugin| plugin.installed_unix_ms),
            last_push,
            ..Self::default()
        }
    }

    pub(crate) fn set_lock(&mut self, snapshot: Option<&str>) {
        self.lock_ref = self.repository.as_ref().and_then(|(repo, _)| {
            let mut records = snapshot?
                .lines()
                .filter_map(entry_record)
                .filter(|(candidate, _)| candidate == repo);
            let (_, reference) = records.next()?;
            if records.next().is_some() {
                return None;
            }
            reference.map(str::to_owned)
        });
        self.consistency =
            self.resolved
                .as_deref()
                .zip(self.locked_revision())
                .map(|(resolved, locked)| {
                    if resolved.eq_ignore_ascii_case(locked) {
                        "Registry revision matches lock"
                    } else {
                        "Registry revision differs from lock"
                    }
                });
    }

    fn locked_revision(&self) -> Option<&str> {
        let locked = self.lock_ref.as_deref().and_then(full_digest)?;
        match self.resolved.as_deref() {
            // With mixed hash formats, show the lockfile ref literally but don't interpret it
            // as a revision of the same checkout.
            Some(resolved) if resolved.len() != locked.len() => None,
            _ => Some(locked),
        }
    }

    pub(crate) fn lines(&self) -> [String; 13] {
        fn fact(label: &str, value: Option<&str>, source: &str) -> String {
            match value {
                Some(value) => format!("{label} · {value} ({source})"),
                None => format!("{label} · {UNKNOWN}"),
            }
        }

        let (repo, repo_source) = self
            .repository
            .as_ref()
            .map_or((None, ""), |(repo, source)| (Some(repo.as_str()), *source));
        let installed_time = self.installed_ms.map(|ms| format!("{ms} Unix ms"));
        [
            fact("Repository", repo, repo_source),
            fact("Requested ref", self.requested.as_deref(), BUNDLE),
            fact("Resolved revision", self.resolved.as_deref(), REGISTRY),
            // The registry supplies one revision, not a second reading of the checkout.
            format!("Installed revision · {UNKNOWN} (checkout not independently inspected)"),
            fact("Lockfile ref", self.lock_ref.as_deref(), LOCK),
            fact("Locked revision", self.locked_revision(), LOCK),
            format!("Lock consistency · {}", self.consistency.unwrap_or(UNKNOWN)),
            fact("Provenance", self.source, REGISTRY),
            fact("Installation recorded", installed_time.as_deref(), REGISTRY),
            fact(
                "Last push",
                self.last_push.as_deref(),
                "Marketplace cache, repository-wide",
            ),
            format!("License, security policy, archived state, external commands · {UNKNOWN}"),
            "Lock comparison does not authenticate the plugin.".to_string(),
            "Provenance uses local snapshots. The separate Changes check may contact GitHub."
                .to_string(),
        ]
    }
}

fn cached_last_push(root: &str, market: &[registry::Entry]) -> Option<String> {
    let mut matching = market
        .iter()
        .filter(|e| e.full_name.eq_ignore_ascii_case(root));
    let pushed_at = &matching.next()?.pushed_at;
    let date = pushed_at.get(..10)?;
    if matching.next().is_some()
        || !matches!(pushed_at.as_bytes().get(10), None | Some(b'T'))
        || !date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return None;
    }
    let status = registry::maintenance_status(date, registry::today_days());
    let age = status.age()?;
    Some(if age == "today" {
        date.to_string()
    } else {
        let quiet = if status.is_quiet() { " · quiet" } else { "" };
        format!("{date} · {age} ago{quiet}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Spec;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn installed(slug: &str, revision: &str) -> Installed {
        Installed {
            plugin_id: "fixture.plugin".into(),
            source_kind: "github".to_string(),
            slug: Some(slug.to_string()),
            resolved_commit: Some(revision.to_string()),
            installed_unix_ms: Some(123),
            ..Installed::default()
        }
    }

    fn bundle(desired: &Spec, installed: &[Installed], market: &[registry::Entry]) -> Report {
        Report::for_bundle(&desired.display(), installed, installed.first(), market).0
    }

    #[test]
    fn reports_local_evidence_without_inventing_a_second_revision() {
        let desired = Spec::parse("Owner/Plugin@v1.2.3+build.4");
        let market = registry::Entry {
            full_name: "OWNER/PLUGIN".into(),
            pushed_at: "2024-01-02T00:00:00Z".into(),
            ..registry::Entry::default()
        };
        let mut report = bundle(&desired, &[installed("owner/PLUGIN", SHA)], &[market]);
        report.set_lock(Some(&format!("git@GITHUB.COM:OWNER/PLUGIN.GIT@{SHA}")));
        let text = report.lines().join("\n");
        for fact in [
            "Repository · owner/plugin (bundle list)",
            "Requested ref · v1.2.3+build.4 (bundle list)",
            &format!("Resolved revision · {SHA} (installed registry)"),
            "Installed revision · Unknown",
            &format!("Locked revision · {SHA} (plugins.lock)"),
            "Registry revision matches lock",
            "Provenance · GitHub (installed registry)",
            "Installation recorded · 123 Unix ms (installed registry)",
            "Last push · 2024-01-02",
            "Marketplace cache, repository-wide",
            "License, security policy, archived state, external commands · Unknown",
            "Lock comparison does not authenticate the plugin.",
            "Provenance uses local snapshots. The separate Changes check may contact GitHub.",
        ] {
            assert!(text.contains(fact), "missing {fact}: {text}");
        }
    }

    #[test]
    fn lock_uses_exact_subdirectory_case_and_rejects_duplicate_identities() {
        let desired = Spec::parse("Owner/Plugin/Tools");
        let mut report = bundle(&desired, &[installed("OWNER/PLUGIN/Tools", SHA)], &[]);
        let first = format!("OWNER/plugin/Tools@{SHA}");
        report.set_lock(Some(&format!("{first}\nowner/plugin/tools@other")));
        assert_eq!(report.lock_ref.as_deref(), Some(SHA));
        assert_eq!(report.consistency, Some("Registry revision matches lock"));

        report.set_lock(Some(&format!("{first}\nowner/plugin/Tools @")));
        assert_eq!(report.lock_ref, None);
        assert_eq!(report.consistency, None);

        report.set_lock(Some(&format!("owner/plugin@{SHA}")));
        assert_eq!(report.lock_ref, None);

        let mut root = Report::for_install(&installed("Owner/Plugin", SHA), &[], &[]).0;
        root.set_lock(Some(&format!(
            "https://github.com/OWNER/plugin.git@{SHA}\nowner/PLUGIN@{SHA}"
        )));
        assert_eq!(root.lock_ref, None);
        root.set_lock(Some("owner/plugin@release/v1"));
        assert!(root
            .lines()
            .contains(&"Lockfile ref · release/v1 (plugins.lock)".into()));
        assert!(root.lines().contains(&"Locked revision · Unknown".into()));
        assert_eq!(root.consistency, None);
    }

    #[test]
    fn registry_url_alias_requires_named_github_source_and_cannot_prove_a_subdir() {
        let plugins = crate::parse_plugin_list(&format!(
            r#"{{"result":{{"plugins":[{{"plugin_id":"url.plugin","source":{{"url":"ssh://git@GitHub.com/OWNER/PLUGIN.git","resolved_commit":"{SHA}"}}}}]}}}}"#
        ))
        .unwrap();
        let root = Spec::parse("owner/plugin");
        let (report, payload) = Report::for_bundle(&root.display(), &plugins, plugins.first(), &[]);
        assert_eq!(report.resolved.as_deref(), Some(SHA));
        assert_eq!(report.source, Some("GitHub URL"));
        assert!(payload.is_some());

        let structured = installed("owner/plugin", SHA);
        for candidates in [
            [plugins[0].clone(), structured.clone()],
            [structured, plugins[0].clone()],
        ] {
            let report = bundle(&root, &candidates, &[]);
            assert_eq!(report.resolved, None);
            assert_eq!(report.source, None);
            assert!(
                Report::for_bundle(&root.display(), &candidates, candidates.first(), &[])
                    .1
                    .is_none()
            );
        }

        let child = Spec::parse("owner/plugin/Tools");
        for plugin in [
            plugins[0].clone(),
            installed("OWNER/PLUGIN", SHA),
            installed("OWNER/PLUGIN/tools", SHA),
        ] {
            let (report, payload) = Report::for_bundle(
                &child.display(),
                std::slice::from_ref(&plugin),
                Some(&plugin),
                &[],
            );
            assert_eq!(report.resolved, None);
            assert_eq!(report.source, None);
            assert!(payload.is_none());
        }
        assert!(repository("https://github.com.evil/owner/plugin").is_none());
        assert!(repository("https://github.com/owner/plugin/tree/main").is_none());
        assert_ne!(repository("owner/plugin.git"), repository("owner/plugin"));
    }

    #[test]
    fn sourced_alias_refs_are_split_after_the_transport() {
        for transport in [
            "git@GitHub.com:Owner/Plugin.git",
            "ssh://git@github.com/Owner/Plugin.git",
            "https://github.com/Owner/Plugin.git",
        ] {
            let plain = Spec::parse(transport);
            let pinned = Spec::parse(&format!("{transport}@v1.2.3+build.4"));
            for (spec, expected_ref) in [(plain, None), (pinned, Some("v1.2.3+build.4"))] {
                let records = [installed("owner/plugin", SHA)];
                let (mut report, payload) =
                    Report::for_bundle(&spec.display(), &records, None, &[]);
                assert_eq!(report.repository.as_ref().unwrap().0, "owner/plugin");
                assert_eq!(report.requested.as_deref(), expected_ref);
                assert_eq!(report.resolved.as_deref(), Some(SHA));
                assert!(payload.is_none());
                report.set_lock(Some(&format!("owner/plugin@{SHA}")));
                assert_eq!(report.consistency, Some("Registry revision matches lock"));
            }
        }
    }

    #[test]
    fn a_named_url_must_corroborate_the_structured_github_root() {
        let desired = Spec::parse("owner/plugin");
        let exact = installed("owner/plugin", SHA);
        for url in [
            "https://github.com/other/plugin",
            "https://example.invalid/owner/plugin.git",
            "https://github.com/owner/plugin/tree/main",
            "not a URL",
            "",
        ] {
            let installed = crate::parse_plugin_list(&format!(
                r#"{{"result":{{"plugins":[{{"source":{{"kind":"github","owner":"owner","repo":"plugin","resolved_commit":"{SHA}","url":"{url}"}}}}]}}}}"#
            ))
            .unwrap();
            let conflict = installed.into_iter().next().unwrap();
            let (direct, payload) =
                Report::for_install(&conflict, std::slice::from_ref(&conflict), &[]);
            assert_eq!(direct.repository, None, "{url:?}");
            assert_eq!(direct.resolved, None, "{url:?}");
            assert!(payload.is_none(), "{url:?}");
            // A contradictory claim for this entry is not repaired by selecting another row.
            let records = [exact.clone(), conflict];
            let (report, payload) =
                Report::for_bundle(&desired.display(), &records, records.first(), &[]);
            assert_eq!(report.resolved, None, "{url:?}");
            assert!(payload.is_none(), "{url:?}");
        }
        let corroborated = Installed {
            source_url: SourceUrl::GitHubRoot("owner/plugin".into()),
            ..exact.clone()
        };
        for plugin in [exact, corroborated] {
            let (report, payload) = Report::for_bundle(
                &desired.display(),
                std::slice::from_ref(&plugin),
                Some(&plugin),
                &[],
            );
            assert_eq!(report.resolved.as_deref(), Some(SHA));
            assert!(payload.is_some());
        }
    }

    #[test]
    fn only_full_digests_of_the_same_format_are_comparable() {
        let short = "0123456";
        let full_64 = "0123456789abcdef".repeat(4);
        let upper = SHA.to_ascii_uppercase();
        let different = "f".repeat(40);
        let report_for = |resolved: &str, locked: &str| {
            let mut report = Report::for_install(&installed("owner/plugin", resolved), &[], &[]).0;
            report.set_lock(Some(&format!("owner/plugin@{locked}")));
            report
        };
        for (resolved, locked) in [(SHA, upper.as_str()), (full_64.as_str(), full_64.as_str())] {
            assert_eq!(
                report_for(resolved, locked).consistency,
                Some("Registry revision matches lock")
            );
        }
        assert_eq!(
            report_for(SHA, &different).consistency,
            Some("Registry revision differs from lock")
        );
        for locked in [short, "fffffff", full_64.as_str()] {
            let report = report_for(SHA, locked);
            assert_eq!(report.lock_ref.as_deref(), Some(locked));
            assert_eq!(report.locked_revision(), None);
            assert_eq!(report.consistency, None);
        }
        let mixed = report_for(&full_64, SHA);
        assert_eq!(mixed.lock_ref.as_deref(), Some(SHA));
        assert_eq!(mixed.locked_revision(), None);
        assert_eq!(mixed.consistency, None);
        for (resolved, locked) in [(short, SHA), ("fffffff", SHA), (short, short)] {
            let report = report_for(resolved, locked);
            assert_eq!(report.resolved, None);
            assert_eq!(report.consistency, None);
        }
        assert!(full_digest(&"a".repeat(65)).is_none());
        assert!(full_digest(&"z".repeat(40)).is_none());
    }

    #[test]
    fn raw_source_values_and_unavailable_or_unmatched_metadata_stay_out() {
        let desired = Spec::parse("owner/plugin@/fixture-private/ref");
        let plugin = Installed {
            name: "plugin".into(),
            source_kind: "local".into(),
            source_values: vec!["https://github.com/owner/plugin".into()],
            source_url: SourceUrl::Invalid,
            resolved_commit: Some("not-a-commit".into()),
            description: "fixture-private".into(),
            ..Installed::default()
        };
        let market = [
            registry::Entry {
                full_name: "other/plugin".into(),
                pushed_at: "2024-01-02T00:00:00Z".into(),
                ..registry::Entry::default()
            },
            registry::Entry {
                full_name: "owner/plugin".into(),
                pushed_at: "2024-02-30T00:00:00Z".into(),
                ..registry::Entry::default()
            },
        ];
        let mut report = bundle(&desired, std::slice::from_ref(&plugin), &market);
        report.set_lock(Some("different/plugin@release"));
        let text = report.lines().join("\n");
        for label in [
            "Requested ref",
            "Resolved revision",
            "Lockfile ref",
            "Lock consistency",
            "Provenance",
            "Last push",
        ] {
            assert!(text.contains(&format!("{label} · Unknown")), "{text}");
        }
        assert!(!text.contains("fixture-private") && !text.contains("credential"));

        let local = Report::for_install(&plugin, std::slice::from_ref(&plugin), &market).0;
        assert_eq!(local.source, Some("Local link"));
        assert_eq!(local.repository, None);
        assert!(safe_ref(&"a".repeat(129)).is_none());
        assert!(repository(&format!("owner/{}", "x".repeat(320))).is_none());
    }

    #[test]
    fn printable_git_refs_are_preserved_without_accepting_paths_or_revision_syntax() {
        for reference in ["v1.2.3+build.4", "refs/tags/v1@rc_2", "release/test%2"] {
            assert_eq!(safe_ref(reference), Some(reference));
        }
        for reference in [
            "/private/repo",
            r"C:\private\repo",
            "a\u{1b}b",
            "feature branch",
            "main~1",
            "main@{1}",
            "v1:tag",
            "refs/tags/.hidden",
            "refs/tags/release.lock",
        ] {
            assert_eq!(safe_ref(reference), None, "{reference:?}");
        }
    }

    #[test]
    fn lock_snapshot_is_never_used_partially() {
        let exact = vec![b'a'; MAX_LOCK_BYTES as usize];
        assert!(read_lock_contents(exact.as_slice()).is_some());
        let oversized = vec![b'a'; MAX_LOCK_BYTES as usize + 1];
        assert!(read_lock_contents(oversized.as_slice()).is_none());
        assert!(read_lock_contents(&[0xff][..]).is_none());
    }

    #[test]
    fn cache_date_is_repository_wide_and_duplicate_entries_are_unknown() {
        let desired = Spec::parse("owner/plugin/Tools");
        let entry = registry::Entry {
            full_name: "Owner/Plugin".into(),
            pushed_at: "2024-01-02T01:02:03Z".into(),
            ..registry::Entry::default()
        };
        let records = [installed("owner/plugin", SHA)];
        let (mut report, payload) = Report::for_bundle(
            &desired.display(),
            &records,
            records.first(),
            std::slice::from_ref(&entry),
        );
        assert!(payload.is_none());
        report.set_lock(Some(&format!("owner/plugin/Tools@{SHA}")));
        assert_eq!(report.locked_revision(), Some(SHA));
        assert_eq!(report.consistency, None);
        assert!(report.lines().iter().any(|line| {
            line.starts_with("Last push · 2024-01-02") && line.contains("repository-wide")
        }));
        let report = bundle(&desired, &[], &[entry.clone(), entry]);
        assert!(report.lines().contains(&"Last push · Unknown".into()));
    }
}
