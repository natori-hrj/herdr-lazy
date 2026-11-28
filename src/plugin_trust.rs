//! Read-only evidence for Plugin Details. Do not reuse operational matching or pin enforcement
//! here: they deliberately tolerate evidence that does not establish provenance.

use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use crate::{registry, Installed, Spec};

const UNKNOWN: &str = "Unknown";
pub(crate) const MAX_IDENTITY_INPUT: usize = 320;
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
        && !value.contains("..")
        && !value.contains("//")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b)))
    .then_some(value)
}

fn commit(value: &str) -> Option<&str> {
    ((7..=64).contains(&value.len()) && crate::is_commit_ref(value)).then_some(value)
}

fn registry_identity(plugin: &Installed) -> Option<(String, &'static str)> {
    if plugin.source_kind == "local" {
        return None;
    }
    let structured = if plugin.source_kind == "github" {
        plugin.slug.as_deref().and_then(repository)
    } else {
        None
    };
    let url = plugin
        .source_url
        .as_deref()
        .filter(|url| strip_alias(url).is_some())
        .and_then(repository);
    if let Some(slug) = structured {
        if url.is_some_and(|url| url != crate::repo_root(&slug)) {
            return None;
        }
        Some((slug, REGISTRY))
    } else {
        url.map(|slug| (slug, "installed registry URL"))
    }
}

fn lock_record(line: &str) -> Option<(String, Option<&str>)> {
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

fn lock_consistency(resolved: &str, locked: &str) -> &'static str {
    let (have_full, lock_full) = (
        matches!(resolved.len(), 40 | 64),
        matches!(locked.len(), 40 | 64),
    );
    // Full hashes in different formats don't establish either equality or drift. An apparent
    // abbreviation longer than a full hash is also not valid evidence in that hash format.
    if (have_full && locked.len() > resolved.len()) || (lock_full && resolved.len() > locked.len())
    {
        return UNKNOWN;
    }
    let shared = resolved.len().min(locked.len());
    if !resolved[..shared].eq_ignore_ascii_case(&locked[..shared]) {
        "Registry revision differs from lock"
    } else if !have_full {
        UNKNOWN
    } else if lock_full {
        "Registry revision matches lock"
    } else {
        "Registry revision matches lock prefix"
    }
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
    pub(crate) fn from_snapshots(
        desired: Option<&Spec>,
        selected_install: Option<&Installed>,
        market: &[registry::Entry],
    ) -> Self {
        let registry_repo = selected_install.and_then(registry_identity);
        let repository = match desired {
            Some(spec) => repository(&spec.repo).map(|slug| (slug, BUNDLE)),
            None => registry_repo.clone(),
        };
        // Existing callers can select by name or a repository-root match. Those choices still
        // drive actions, but can't supply revision evidence for another plugin in a monorepo.
        let installed = selected_install.filter(|_| {
            desired.is_none()
                || repository
                    .as_ref()
                    .zip(registry_repo.as_ref())
                    .is_some_and(|((wanted, _), (recorded, _))| wanted == recorded)
        });
        let requested = desired
            .and_then(|spec| spec.reference.as_deref())
            .and_then(safe_ref);
        let last_push = repository
            .as_ref()
            .and_then(|(slug, _)| cached_last_push(&crate::repo_root(slug), market));
        let source = installed.and_then(|plugin| match plugin.source_kind.as_str() {
            "github" => Some("GitHub"),
            "local" => Some("Local link"),
            _ if registry_repo.is_some() => Some("GitHub URL"),
            _ => None,
        });
        Self {
            repository,
            requested: requested.map(str::to_owned),
            resolved: installed
                .and_then(|plugin| plugin.resolved_commit.as_deref())
                .and_then(commit)
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
                .filter_map(lock_record)
                .filter(|(candidate, _)| candidate == repo);
            let (_, reference) = records.next()?;
            if records.next().is_some() {
                return None;
            }
            reference.map(str::to_owned)
        });
        self.consistency = self.resolved.as_deref().and_then(|resolved| {
            let locked = self.lock_ref.as_deref().and_then(commit)?;
            Some(lock_consistency(resolved, locked))
        });
    }

    pub(crate) fn lines(&self) -> [String; 12] {
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
            fact(
                "Locked revision",
                self.lock_ref.as_deref().and_then(commit),
                LOCK,
            ),
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

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn installed(slug: &str, revision: &str) -> Installed {
        Installed {
            source_kind: "github".to_string(),
            slug: Some(slug.to_string()),
            resolved_commit: Some(revision.to_string()),
            installed_unix_ms: Some(123),
            ..Installed::default()
        }
    }

    #[test]
    fn reports_local_evidence_without_inventing_a_second_revision() {
        let desired = Spec::parse("Owner/Plugin@stable");
        let market = registry::Entry {
            full_name: "OWNER/PLUGIN".into(),
            pushed_at: "2024-01-02T00:00:00Z".into(),
            ..registry::Entry::default()
        };
        let mut report = Report::from_snapshots(
            Some(&desired),
            Some(&installed("owner/PLUGIN", SHA)),
            &[market],
        );
        report.set_lock(Some(&format!("git@GITHUB.COM:OWNER/PLUGIN.GIT@{SHA}")));
        let text = report.lines().join("\n");
        for fact in [
            "Repository · owner/plugin (bundle list)",
            "Requested ref · stable (bundle list)",
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
        ] {
            assert!(text.contains(fact), "missing {fact}: {text}");
        }
    }

    #[test]
    fn lock_uses_exact_subdirectory_case_and_rejects_duplicate_identities() {
        let desired = Spec::parse("Owner/Plugin/Tools");
        let mut report = Report::from_snapshots(
            Some(&desired),
            Some(&installed("OWNER/PLUGIN/Tools", SHA)),
            &[],
        );
        let first = format!("OWNER/plugin/Tools@{SHA}");
        report.set_lock(Some(&format!("{first}\nowner/plugin/tools@other")));
        assert_eq!(report.lock_ref.as_deref(), Some(SHA));
        assert_eq!(report.consistency, Some("Registry revision matches lock"));

        report.set_lock(Some(&format!("{first}\nowner/plugin/Tools @")));
        assert_eq!(report.lock_ref, None);
        assert_eq!(report.consistency, None);

        report.set_lock(Some(&format!("owner/plugin@{SHA}")));
        assert_eq!(report.lock_ref, None);

        let mut root = Report::from_snapshots(None, Some(&installed("Owner/Plugin", SHA)), &[]);
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
            r#"{{"result":{{"plugins":[{{"source":{{"url":"ssh://git@GitHub.com/OWNER/PLUGIN.git","resolved_commit":"{SHA}"}}}}]}}}}"#
        ))
        .unwrap();
        let root = Spec::parse("owner/plugin");
        let report = Report::from_snapshots(Some(&root), plugins.first(), &[]);
        assert_eq!(report.resolved.as_deref(), Some(SHA));
        assert_eq!(report.source, Some("GitHub URL"));

        let child = Spec::parse("owner/plugin/Tools");
        for plugin in [
            plugins[0].clone(),
            installed("OWNER/PLUGIN", SHA),
            installed("OWNER/PLUGIN/tools", SHA),
        ] {
            let report = Report::from_snapshots(Some(&child), Some(&plugin), &[]);
            assert_eq!(report.resolved, None);
            assert_eq!(report.source, None);
        }
        let conflict = Installed {
            source_url: Some("https://github.com/other/plugin".into()),
            ..installed("owner/plugin", SHA)
        };
        assert!(registry_identity(&conflict).is_none());
        assert!(repository("https://github.com.evil/owner/plugin").is_none());
        assert!(repository("https://github.com/owner/plugin/tree/main").is_none());
        assert_ne!(repository("owner/plugin.git"), repository("owner/plugin"));
    }

    #[test]
    fn full_hashes_match_exactly_and_short_hashes_prove_only_prefix_or_difference() {
        let short = "0123456";
        let full_64 = "0123456789abcdef".repeat(4);
        assert_eq!(
            lock_consistency(SHA, &SHA.to_ascii_uppercase()),
            "Registry revision matches lock"
        );
        assert_eq!(
            lock_consistency(&full_64, &full_64),
            "Registry revision matches lock"
        );
        assert_eq!(lock_consistency(SHA, &full_64), UNKNOWN);
        assert_eq!(
            lock_consistency(SHA, short),
            "Registry revision matches lock prefix"
        );
        assert_eq!(lock_consistency(short, SHA), UNKNOWN);
        assert_eq!(lock_consistency(short, short), UNKNOWN);
        assert_eq!(
            lock_consistency("0123457", SHA),
            "Registry revision differs from lock"
        );
        assert!(commit(&"a".repeat(65)).is_none());
    }

    #[test]
    fn raw_source_values_and_unavailable_or_unmatched_metadata_stay_out() {
        let desired = Spec::parse("owner/plugin@/fixture-private/ref");
        let plugin = Installed {
            name: "plugin".into(),
            source_kind: "local".into(),
            source_values: vec!["https://github.com/owner/plugin".into()],
            source_url: Some("https://credential@github.com/owner/plugin".into()),
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
        let mut report = Report::from_snapshots(Some(&desired), Some(&plugin), &market);
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

        let local = Report::from_snapshots(None, Some(&plugin), &market);
        assert_eq!(local.source, Some("Local link"));
        assert_eq!(local.repository, None);
        assert!(safe_ref(&"a".repeat(129)).is_none());
        assert!(repository(&format!("owner/{}", "x".repeat(320))).is_none());
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
        let report = Report::from_snapshots(Some(&desired), None, std::slice::from_ref(&entry));
        assert!(report.lines().iter().any(|line| {
            line.starts_with("Last push · 2024-01-02") && line.contains("repository-wide")
        }));
        let report = Report::from_snapshots(Some(&desired), None, &[entry.clone(), entry]);
        assert!(report.lines().contains(&"Last push · Unknown".into()));
    }
}
