//! Workspace-scoped plugin profiles.
//!
//! A profile is deliberately a small, plain-text overlay.  It is discovered only from the
//! workspace path Herdr supplied for the current pane; we do not walk parent directories or use
//! the process cwd as a fallback.  That keeps a repository checkout from unexpectedly inheriting
//! a profile from one of its parents.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{context::PluginContext, Spec};

pub(crate) const PROFILE_DIRECTORY: &str = ".herdr-lazy";
pub(crate) const PROFILE_LIST_NAME: &str = "plugins.list";
pub(crate) const PROFILE_LOCK_NAME: &str = "plugins.lock";

const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAX_PROFILE_ENTRIES: usize = 512;

/// The state of the optional lockfile beside a workspace profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LockStatus {
    Missing,
    Ready(usize),
    Invalid(String),
}

/// A valid project-scoped plugin list and the lockfile beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceProfile {
    pub(crate) root: PathBuf,
    pub(crate) list_path: PathBuf,
    pub(crate) lock_path: PathBuf,
    pub(crate) specs: Vec<Spec>,
    pub(crate) lock_status: LockStatus,
}

/// The reviewable difference between the project profile and the global selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProfileDiff {
    /// Entries the profile adds which have no global counterpart.
    pub(crate) additions: Vec<Spec>,
    /// The same repository exists in both places, but the requested pin differs.
    pub(crate) pin_changes: Vec<(Spec, Spec)>, // (global, profile)
    /// Global entries not selected by the profile. They are displayed, never removed by profile
    /// sync; this is the safety boundary between a project overlay and the global list.
    pub(crate) global_only: Vec<Spec>,
    pub(crate) unchanged: usize,
}

impl ProfileDiff {
    pub(crate) fn is_empty(&self) -> bool {
        self.additions.is_empty() && self.pin_changes.is_empty() && self.global_only.is_empty()
    }
}

/// Find the profile for the current Herdr workspace.
pub(crate) fn discover(
    context: Option<&PluginContext>,
) -> Result<Option<WorkspaceProfile>, String> {
    let Some(context) = context else {
        return Ok(None);
    };

    // `workspace_cwd` is the authoritative project location. `worktree_path` is the fallback
    // used by older Herdr payloads that identify the checkout but omit the workspace cwd.
    let roots = context
        .workspace_cwd
        .as_deref()
        .into_iter()
        .chain(context.worktree_path.as_deref())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();
    for root in roots {
        if let Some(profile) = discover_at(&root)? {
            return Ok(Some(profile));
        }
    }
    Ok(None)
}

/// Discover a profile at one exact workspace root.
///
/// Kept separate from the context adapter so path and malformed-file behaviour can be tested
/// without manufacturing Herdr's environment payload.
pub(crate) fn discover_at(root: &Path) -> Result<Option<WorkspaceProfile>, String> {
    let directory = root.join(PROFILE_DIRECTORY);
    let list_path = directory.join(PROFILE_LIST_NAME);
    let lock_path = directory.join(PROFILE_LOCK_NAME);

    let metadata = match fs::metadata(&list_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not inspect workspace profile {}: {}",
                list_path.display(),
                error
            ))
        }
    };
    if !metadata.is_file() {
        return Err(format!(
            "workspace profile {} is not a file",
            list_path.display()
        ));
    }

    let specs = read_specs(&list_path, "workspace profile")?;
    let lock_status = match fs::metadata(&lock_path) {
        Ok(metadata) if !metadata.is_file() => {
            LockStatus::Invalid(format!("{} is not a file", lock_path.display()))
        }
        Ok(_) => match read_specs(&lock_path, "workspace profile lock") {
            Ok(lock_specs) if same_repositories(&specs, &lock_specs) => {
                LockStatus::Ready(lock_specs.len())
            }
            Ok(lock_specs) => LockStatus::Invalid(format!(
                "{} entries do not match the {} profile entries",
                lock_specs.len(),
                specs.len()
            )),
            Err(error) => LockStatus::Invalid(error),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LockStatus::Missing,
        Err(error) => LockStatus::Invalid(format!(
            "could not inspect {}: {}",
            lock_path.display(),
            error
        )),
    };

    Ok(Some(WorkspaceProfile {
        root: root.to_path_buf(),
        list_path,
        lock_path,
        specs,
        lock_status,
    }))
}

/// Compare entries by repository identity, while still surfacing a changed pin as a separate
/// review item. A profile should never produce two actions for one repository.
pub(crate) fn diff(profile: &[Spec], global: &[Spec]) -> ProfileDiff {
    let mut out = ProfileDiff::default();

    for wanted in profile {
        match global.iter().find(|existing| existing.repo == wanted.repo) {
            None => out.additions.push(wanted.clone()),
            Some(existing) if existing.display() == wanted.display() => out.unchanged += 1,
            Some(existing) => out.pin_changes.push((existing.clone(), wanted.clone())),
        }
    }

    for existing in global {
        if !profile.iter().any(|wanted| wanted.repo == existing.repo) {
            out.global_only.push(existing.clone());
        }
    }

    out
}

fn same_repositories(profile: &[Spec], lock: &[Spec]) -> bool {
    profile.len() == lock.len()
        && profile
            .iter()
            .all(|wanted| lock.iter().any(|recorded| recorded.repo == wanted.repo))
}

fn read_specs(path: &Path, label: &str) -> Result<Vec<Spec>, String> {
    let bytes = fs::read(path).map_err(|error| format!("could not read {}: {}", label, error))?;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(format!(
            "{} {} exceeds {} bytes",
            label,
            path.display(),
            MAX_PROFILE_BYTES
        ));
    }
    let body = String::from_utf8(bytes)
        .map_err(|_| format!("{} {} is not valid UTF-8", label, path.display()))?;

    let mut specs = Vec::new();
    for (line_number, raw) in body.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if specs.len() >= MAX_PROFILE_ENTRIES {
            return Err(format!(
                "{} {} contains more than {} entries",
                label,
                path.display(),
                MAX_PROFILE_ENTRIES
            ));
        }
        let spec = parse_spec(line).map_err(|reason| {
            format!(
                "{} {} line {}: {}",
                label,
                path.display(),
                line_number + 1,
                reason
            )
        })?;
        if specs
            .iter()
            .any(|existing: &Spec| existing.repo == spec.repo)
        {
            return Err(format!(
                "{} {} line {}: duplicate repository `{}`",
                label,
                path.display(),
                line_number + 1,
                spec.repo
            ));
        }
        specs.push(spec);
    }
    Ok(specs)
}

fn parse_spec(line: &str) -> Result<Spec, &'static str> {
    if line
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err("entries cannot contain whitespace");
    }
    let (repo, reference) = match line.split_once('@') {
        Some((repo, reference)) if !repo.is_empty() && !reference.is_empty() => {
            (repo, Some(reference))
        }
        Some(_) => return Err("a pin must have both a repository and a reference"),
        None => (line, None),
    };
    if repo.contains('\\') {
        return Err("use owner/repo paths, not backslashes");
    }
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() < 2
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err("expected owner/repo or owner/repo/subdir");
    }

    Ok(Spec {
        repo: repo.to_string(),
        reference: reference.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "herdr-lazy-profile-{}-{}",
            std::process::id(),
            name
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(PROFILE_DIRECTORY)).unwrap();
        root
    }

    fn clean(root: &Path) {
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_profile_and_lock_beside_the_workspace_list() {
        let root = scratch("discover");
        let directory = root.join(PROFILE_DIRECTORY);
        fs::write(
            directory.join(PROFILE_LIST_NAME),
            "# project tools\nowner/a@1111111\nowner/b\n",
        )
        .unwrap();
        fs::write(
            directory.join(PROFILE_LOCK_NAME),
            "owner/a@2222222\nowner/b@3333333\n",
        )
        .unwrap();

        let profile = discover_at(&root).unwrap().unwrap();
        assert_eq!(profile.root, root);
        assert_eq!(profile.specs.len(), 2);
        assert_eq!(profile.lock_status, LockStatus::Ready(2));
        clean(&root);
    }

    #[test]
    fn a_lock_that_does_not_cover_the_profile_is_not_ready_to_restore() {
        let root = scratch("mismatched-lock");
        let directory = root.join(PROFILE_DIRECTORY);
        fs::write(directory.join(PROFILE_LIST_NAME), "owner/a\nowner/b\n").unwrap();
        fs::write(directory.join(PROFILE_LOCK_NAME), "owner/a@2222222\n").unwrap();

        let profile = discover_at(&root).unwrap().unwrap();
        assert!(matches!(profile.lock_status, LockStatus::Invalid(_)));
        clean(&root);
    }

    #[test]
    fn malformed_profile_fails_closed() {
        let root = scratch("malformed");
        fs::write(
            root.join(PROFILE_DIRECTORY).join(PROFILE_LIST_NAME),
            "owner/a\nthis is not a plugin\n",
        )
        .unwrap();

        let error = discover_at(&root).unwrap_err();
        assert!(error.contains("line 2"));
        assert!(error.contains("whitespace"));
        clean(&root);
    }

    #[test]
    fn profile_diff_keeps_global_only_entries_as_non_destructive_information() {
        let profile = vec![Spec::parse("owner/a@new"), Spec::parse("owner/b")];
        let global = vec![
            Spec::parse("owner/a@old"),
            Spec::parse("owner/c"),
            Spec::parse("owner/b"),
        ];

        let changes = diff(&profile, &global);
        assert!(changes.additions.is_empty());
        assert_eq!(changes.pin_changes.len(), 1);
        assert_eq!(changes.global_only, vec![Spec::parse("owner/c")]);
        assert_eq!(changes.unchanged, 1);
    }

    #[test]
    fn relative_context_paths_are_not_used_for_discovery() {
        let context = PluginContext {
            workspace_cwd: Some("relative/project".to_string()),
            ..Default::default()
        };
        assert_eq!(discover(Some(&context)).unwrap(), None);
    }

    #[test]
    fn worktree_path_is_used_when_workspace_cwd_has_no_profile() {
        let root = scratch("worktree-fallback");
        fs::write(
            root.join(PROFILE_DIRECTORY).join(PROFILE_LIST_NAME),
            "owner/project-tool\n",
        )
        .unwrap();
        let context = PluginContext {
            workspace_cwd: Some(root.join("nested").display().to_string()),
            worktree_path: Some(root.display().to_string()),
            ..Default::default()
        };

        let profile = discover(Some(&context)).unwrap().unwrap();
        assert_eq!(profile.root, root);
        clean(&root);
    }
}
