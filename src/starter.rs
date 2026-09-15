//! The workspace starter's planning and launch-safety rules.
//!
//! This module deliberately knows nothing about terminals or keymaps.  It turns a selected
//! profile/bundle plus Herdr's installed snapshot into a reviewable plan, and it re-validates
//! launch targets against that snapshot immediately before invoking them.  Repository profile
//! files are data here — never commands.

use std::path::PathBuf;

use crate::{registry, Installed, Match, PinState, Spec};

/// Which declarative selection the starter is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Profile,
    Global,
}

/// A list and the lockfile it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) kind: SourceKind,
    pub(crate) list_path: PathBuf,
    pub(crate) lock_path: PathBuf,
    pub(crate) specs: Vec<Spec>,
}

impl Source {
    pub(crate) fn global(list_path: PathBuf, lock_path: PathBuf, specs: Vec<Spec>) -> Self {
        Self {
            kind: SourceKind::Global,
            list_path,
            lock_path,
            specs,
        }
    }

    pub(crate) fn profile(list_path: PathBuf, lock_path: PathBuf, specs: Vec<Spec>) -> Self {
        Self {
            kind: SourceKind::Profile,
            list_path,
            lock_path,
            specs,
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self.kind {
            SourceKind::Profile => "workspace profile",
            SourceKind::Global => "global bundle",
        }
    }
}

/// What the starter found for one selected entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItemState {
    Ready,
    Missing,
    Drifted { have: String },
    UpdateAvailable,
    Disabled,
    Unverifiable,
}

impl ItemState {
    pub(crate) fn marker(&self) -> &'static str {
        match self {
            Self::Ready => "✔",
            Self::Missing => "✗",
            Self::Drifted { .. } => "↻",
            Self::UpdateAvailable => "↑",
            Self::Disabled => "○",
            Self::Unverifiable => "?",
        }
    }

    pub(crate) fn colour(&self) -> &'static str {
        match self {
            Self::Ready => "\x1b[32m",
            Self::Missing | Self::Drifted { .. } | Self::UpdateAvailable => "\x1b[33m",
            Self::Disabled | Self::Unverifiable => "\x1b[36m",
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Ready => "ready".to_string(),
            Self::Missing => "missing".to_string(),
            Self::Drifted { have } => format!("drifted (installed {})", crate::short(have)),
            Self::UpdateAvailable => "update available".to_string(),
            Self::Disabled => "installed but disabled".to_string(),
            Self::Unverifiable => "pinned to a tag/branch; cannot verify locally".to_string(),
        }
    }

    pub(crate) fn needs_install(&self) -> bool {
        matches!(self, Self::Missing | Self::Drifted { .. })
    }

    pub(crate) fn needs_update(&self) -> bool {
        matches!(self, Self::UpdateAvailable)
    }
}

/// One selected entry and the state the preview found for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanItem {
    pub(crate) spec: Spec,
    pub(crate) state: ItemState,
}

/// The complete, read-only preview shown before any install or update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) source: Source,
    pub(crate) items: Vec<PlanItem>,
}

/// A manifest-declared thing the starter may ask Herdr to launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchKind {
    Action,
    Pane,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchTarget {
    pub(crate) plugin_id: String,
    pub(crate) entrypoint: String,
    pub(crate) title: String,
    pub(crate) kind: LaunchKind,
}

fn best_match<'a>(spec: &Spec, installed: &'a [Installed]) -> Option<(&'a Installed, Match)> {
    installed
        .iter()
        .map(|plugin| (plugin, plugin.matches(spec)))
        .filter(|(_, matched)| *matched != Match::None)
        .max_by_key(|(_, matched)| (*matched == Match::Strong) as u8)
}

fn may_have_update(plugin: &Installed, spec: &Spec, market: &[registry::Entry]) -> bool {
    // A pinned entry is intentionally stationary, even if its repository moved.
    if spec.reference.is_some() {
        return false;
    }
    let (Some(slug), Some(installed_at)) = (plugin.slug.as_ref(), plugin.installed_unix_ms) else {
        return false;
    };
    market
        .iter()
        .find(|entry| entry.full_name.eq_ignore_ascii_case(slug))
        .map(|entry| registry::pushed_since(&entry.pushed_at, installed_at))
        .unwrap_or(false)
}

/// Build the preview without performing any I/O or changing state.
pub(crate) fn build_plan(
    source: &Source,
    installed: &[Installed],
    market: &[registry::Entry],
) -> Plan {
    let items = source
        .specs
        .iter()
        .map(|spec| {
            let state = match best_match(spec, installed) {
                None => ItemState::Missing,
                Some((plugin, _)) => match crate::pin_state(spec, plugin) {
                    PinState::Drifted { have } => ItemState::Drifted { have },
                    PinState::Unverifiable if spec.reference.is_some() => ItemState::Unverifiable,
                    _ if !plugin.enabled => ItemState::Disabled,
                    _ if may_have_update(plugin, spec, market) => ItemState::UpdateAvailable,
                    _ => ItemState::Ready,
                },
            };
            PlanItem {
                spec: spec.clone(),
                state,
            }
        })
        .collect();

    Plan {
        source: source.clone(),
        items,
    }
}

fn title_or_id(title: &str, id: &str) -> String {
    if title.trim().is_empty() {
        id.to_string()
    } else {
        title.to_string()
    }
}

/// Return only targets that belong to an installed, enabled plugin matched authoritatively by
/// the selected repository. Weak name-only matches are intentionally not launchable: opening a
/// command from the wrong plugin would violate the starter's safety boundary.
pub(crate) fn launch_targets(specs: &[Spec], installed: &[Installed]) -> Vec<LaunchTarget> {
    let mut targets = Vec::new();
    for plugin in installed {
        if crate::is_self_id(&plugin.plugin_id) || !plugin.enabled {
            continue;
        }
        if !specs
            .iter()
            .any(|spec| plugin.matches(spec) == Match::Strong)
        {
            continue;
        }

        for (entrypoint, title) in &plugin.actions {
            if entrypoint.is_empty() {
                continue;
            }
            let target = LaunchTarget {
                plugin_id: plugin.plugin_id.clone(),
                entrypoint: entrypoint.clone(),
                title: title_or_id(title, entrypoint),
                kind: LaunchKind::Action,
            };
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        for (entrypoint, title, _) in &plugin.panes {
            if entrypoint.is_empty() {
                continue;
            }
            let target = LaunchTarget {
                plugin_id: plugin.plugin_id.clone(),
                entrypoint: entrypoint.clone(),
                title: title_or_id(title, entrypoint),
                kind: LaunchKind::Pane,
            };
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
    targets
}

/// Re-check a selected target immediately before launching it.
pub(crate) fn launch_allowed(
    target: &LaunchTarget,
    specs: &[Spec],
    installed: &[Installed],
) -> bool {
    let Some(plugin) = installed
        .iter()
        .find(|plugin| plugin.plugin_id == target.plugin_id)
    else {
        return false;
    };
    if crate::is_self_id(&plugin.plugin_id)
        || !plugin.enabled
        || !specs
            .iter()
            .any(|spec| plugin.matches(spec) == Match::Strong)
    {
        return false;
    }

    match target.kind {
        LaunchKind::Action => plugin
            .actions
            .iter()
            .any(|(id, _)| id == &target.entrypoint),
        LaunchKind::Pane => plugin
            .panes
            .iter()
            .any(|(id, _, _)| id == &target.entrypoint),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: &str = "f32b0825f12543c1d03e54fb10d1741c40d66cdc";

    fn source(entries: &[&str]) -> Source {
        Source::global(
            PathBuf::from("/config/plugins.list"),
            PathBuf::from("/config/plugins.lock"),
            entries.iter().map(|entry| Spec::parse(entry)).collect(),
        )
    }

    fn github(owner: &str, repo: &str, commit: &str, enabled: bool) -> Installed {
        Installed {
            plugin_id: repo.to_string(),
            name: repo.to_string(),
            enabled,
            source_kind: "github".to_string(),
            slug: Some(format!("{}/{}", owner, repo)),
            resolved_commit: Some(commit.to_string()),
            source_values: vec![owner.to_string(), repo.to_string()],
            ..Default::default()
        }
    }

    fn market(full_name: &str, pushed_at: &str) -> registry::Entry {
        registry::Entry {
            full_name: full_name.to_string(),
            pushed_at: pushed_at.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn plan_distinguishes_missing_drift_update_and_disabled() {
        let source = source(&[
            "owner/missing",
            "owner/drifted@f32b082",
            "owner/stale",
            "owner/disabled",
        ]);
        let mut drifted = github(
            "owner",
            "drifted",
            "a8f86ec4103bc367b52e547b492483f3b792a952",
            true,
        );
        drifted.source_values.push("owner/drifted".to_string());
        let mut stale = github("owner", "stale", PIN, true);
        stale.installed_unix_ms = Some(1);
        let installed = vec![drifted, stale, github("owner", "disabled", PIN, false)];
        let plan = build_plan(
            &source,
            &installed,
            &[market("owner/stale", "2026-01-01T00:00:00Z")],
        );

        assert!(matches!(plan.items[0].state, ItemState::Missing));
        assert!(matches!(plan.items[1].state, ItemState::Drifted { .. }));
        assert!(matches!(plan.items[2].state, ItemState::UpdateAvailable));
        assert!(matches!(plan.items[3].state, ItemState::Disabled));
    }

    #[test]
    fn launch_targets_are_manifest_declared_and_strongly_matched() {
        let mut plugin = github("owner", "ready", PIN, true);
        plugin.actions = vec![("open-action".to_string(), "Open it".to_string())];
        plugin.panes = vec![(
            "main-pane".to_string(),
            "Main".to_string(),
            "overlay".to_string(),
        )];
        let mut weak = Installed {
            plugin_id: "wrong-id".to_string(),
            name: "ready".to_string(),
            enabled: true,
            actions: vec![("wrong-action".to_string(), "Wrong".to_string())],
            ..Default::default()
        };
        weak.source_values.push("unrelated/source".to_string());
        let disabled = github("owner", "disabled", PIN, false);
        let self_plugin = Installed {
            plugin_id: "herdr-lazy".to_string(),
            name: "herdr-lazy".to_string(),
            enabled: true,
            actions: vec![("starter".to_string(), "Starter".to_string())],
            ..Default::default()
        };
        let targets = launch_targets(
            &[Spec::parse("owner/ready")],
            &[plugin, weak, disabled, self_plugin],
        );

        assert_eq!(targets.len(), 2);
        assert!(targets
            .iter()
            .any(|target| target.entrypoint == "open-action"));
        assert!(targets
            .iter()
            .any(|target| target.entrypoint == "main-pane"));
        assert!(!targets
            .iter()
            .any(|target| target.entrypoint == "wrong-action"));
    }

    #[test]
    fn launch_allowed_rejects_changed_manifest_and_state() {
        let mut plugin = github("owner", "ready", PIN, true);
        plugin.actions = vec![("open-action".to_string(), "Open it".to_string())];
        let target = LaunchTarget {
            plugin_id: "ready".to_string(),
            entrypoint: "open-action".to_string(),
            title: "Open it".to_string(),
            kind: LaunchKind::Action,
        };
        assert!(launch_allowed(
            &target,
            &[Spec::parse("owner/ready")],
            &[plugin.clone()]
        ));

        plugin.actions.clear();
        assert!(!launch_allowed(
            &target,
            &[Spec::parse("owner/ready")],
            &[plugin.clone()]
        ));
        plugin
            .actions
            .push(("open-action".to_string(), "Open it".to_string()));
        plugin.enabled = false;
        assert!(!launch_allowed(
            &target,
            &[Spec::parse("owner/ready")],
            &[plugin]
        ));
    }
}
