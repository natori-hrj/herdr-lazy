//! The invocation context and lifecycle events Herdr gives to plugins.
//!
//! Herdr passes a point-in-time JSON snapshot rather than a live stream.  That makes it a good
//! source for labelling the manage pane, but not a reason to change the user's plugin set.  The
//! event hook below records only the last worktree lifecycle event in plugin-owned state; the
//! pane uses it as a quiet, workspace-matched hint.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use crate::json::{self, Value};

const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_FIELD_CHARS: usize = 1024;
const MAX_SUMMARY_CHARS: usize = 140;
const LAST_EVENT_FILE: &str = "last-workspace-event.json";

/// The subset of Herdr's invocation context that is useful to herdr-lazy today.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PluginContext {
    pub(crate) workspace_id: Option<String>,
    pub(crate) workspace_label: Option<String>,
    pub(crate) workspace_cwd: Option<String>,
    pub(crate) tab_id: Option<String>,
    pub(crate) tab_label: Option<String>,
    pub(crate) focused_pane_id: Option<String>,
    pub(crate) focused_pane_cwd: Option<String>,
    pub(crate) focused_pane_agent: Option<String>,
    pub(crate) focused_pane_status: Option<String>,
    pub(crate) worktree_branch: Option<String>,
    pub(crate) worktree_path: Option<String>,
}

impl PluginContext {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Fill missing fields from a lower-priority source without overwriting known values.
    fn with_fallback(&self, fallback: &Self) -> Self {
        Self {
            workspace_id: self
                .workspace_id
                .clone()
                .or_else(|| fallback.workspace_id.clone()),
            workspace_label: self
                .workspace_label
                .clone()
                .or_else(|| fallback.workspace_label.clone()),
            workspace_cwd: self
                .workspace_cwd
                .clone()
                .or_else(|| fallback.workspace_cwd.clone()),
            tab_id: self.tab_id.clone().or_else(|| fallback.tab_id.clone()),
            tab_label: self
                .tab_label
                .clone()
                .or_else(|| fallback.tab_label.clone()),
            focused_pane_id: self
                .focused_pane_id
                .clone()
                .or_else(|| fallback.focused_pane_id.clone()),
            focused_pane_cwd: self
                .focused_pane_cwd
                .clone()
                .or_else(|| fallback.focused_pane_cwd.clone()),
            focused_pane_agent: self
                .focused_pane_agent
                .clone()
                .or_else(|| fallback.focused_pane_agent.clone()),
            focused_pane_status: self
                .focused_pane_status
                .clone()
                .or_else(|| fallback.focused_pane_status.clone()),
            worktree_branch: self
                .worktree_branch
                .clone()
                .or_else(|| fallback.worktree_branch.clone()),
            worktree_path: self
                .worktree_path
                .clone()
                .or_else(|| fallback.worktree_path.clone()),
        }
    }

    /// A short, control-free label for the manage pane's optional context summary.
    pub(crate) fn summary(&self) -> Option<String> {
        let workspace = self
            .workspace_label
            .as_deref()
            .or(self.workspace_id.as_deref())?;
        let workspace = self
            .workspace_id
            .as_deref()
            .map(|id| format!("{} ({})", workspace, id))
            .unwrap_or_else(|| workspace.to_string());

        let mut parts = vec![format!("workspace {}", workspace)];
        if let Some(cwd) = self
            .workspace_cwd
            .as_deref()
            .or(self.focused_pane_cwd.as_deref())
        {
            parts.push(format!("cwd {}", cwd));
        }
        if let Some(branch) = self.worktree_branch.as_deref() {
            parts.push(format!("branch {}", branch));
        }
        if let Some(agent) = self.focused_pane_agent.as_deref() {
            let status = self
                .focused_pane_status
                .as_deref()
                .map(|value| format!(" ({})", value))
                .unwrap_or_default();
            parts.push(format!("agent {}{}", agent, status));
        }
        Some(truncate(&parts.join(" · "), MAX_SUMMARY_CHARS))
    }
}

/// The context state used by the UI.  A malformed JSON payload does not prevent the pane from
/// opening; individual Herdr id variables remain a useful fallback, and the warning is shown as
/// a small header notice when no usable workspace context remains.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContextSnapshot {
    pub(crate) context: Option<PluginContext>,
    pub(crate) warning: Option<String>,
}

/// A Herdr event plus the context attached to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginEvent {
    pub(crate) name: String,
    pub(crate) context: PluginContext,
}

/// Only these two lifecycle events are useful to the first workspace-aware feature.  A focus
/// hook would run on every workspace switch and turn a quiet hint into constant state writes.
pub(crate) fn is_tracked_event(name: &str) -> bool {
    matches!(name, "worktree.created" | "worktree.opened")
}

/// Read Herdr's point-in-time invocation context, using the individual ids as a fallback for
/// older or partial invocations.
pub(crate) fn read_context_from_env() -> ContextSnapshot {
    let fallback = context_from_env_vars();
    let raw = match env::var("HERDR_PLUGIN_CONTEXT_JSON") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            return ContextSnapshot {
                context: (!fallback.is_empty()).then_some(fallback),
                warning: None,
            }
        }
    };

    match parse_context_json(&raw) {
        Ok(context) => {
            let context = context.with_fallback(&fallback);
            ContextSnapshot {
                context: (!context.is_empty()).then_some(context),
                warning: None,
            }
        }
        Err(error) => ContextSnapshot {
            context: (!fallback.is_empty()).then_some(fallback),
            warning: Some(error),
        },
    }
}

/// Parse a context JSON object without consulting process environment variables.
pub(crate) fn parse_context_json(input: &str) -> Result<PluginContext, String> {
    let value = parse_object(input, "HERDR_PLUGIN_CONTEXT_JSON")?;
    Ok(context_from_value(&value, None))
}

/// Parse an event payload.  Herdr wraps event data under `data`; accepting the flat form too
/// keeps this tolerant of small payload-shape changes and makes the degradation explicit.
pub(crate) fn parse_event_json(input: &str) -> Result<PluginContext, String> {
    if input.trim().is_empty() {
        return Ok(PluginContext::default());
    }
    let value = parse_object(input, "HERDR_PLUGIN_EVENT_JSON")?;
    let data = value.get("data");
    Ok(context_from_value(&value, data))
}

/// Turn one event invocation into a recordable event.  Unknown events and events without a
/// workspace are deliberately ignored: neither can improve the manage view, and recording them
/// would make an old or partial payload look authoritative.
pub(crate) fn parse_event(
    event_name: &str,
    event_json: Option<&str>,
    context: &PluginContext,
) -> Result<Option<PluginEvent>, String> {
    let name = event_name.trim();
    if !is_tracked_event(name) {
        return Ok(None);
    }
    let payload = match event_json {
        Some(json) => parse_event_json(json)?,
        None => PluginContext::default(),
    };
    let context = context.with_fallback(&payload);
    if context.workspace_id.is_none() {
        return Ok(None);
    }
    Ok(Some(PluginEvent {
        name: name.to_string(),
        context,
    }))
}

/// Read the environment supplied to an event hook.
pub(crate) fn event_from_env() -> Result<Option<PluginEvent>, String> {
    let Some(name) = env_value("HERDR_PLUGIN_EVENT") else {
        return Ok(None);
    };
    let snapshot = read_context_from_env();
    let context = snapshot.context.unwrap_or_default();
    let event_json = env::var("HERDR_PLUGIN_EVENT_JSON").ok();
    parse_event(&name, event_json.as_deref(), &context)
}

/// Persist the last supported event in Herdr's plugin-owned state directory.
///
/// `Ok(false)` means the command was run outside Herdr and no state directory was available.
/// That is a normal development/shell invocation, not an error.
pub(crate) fn persist_last_event(event: &PluginEvent) -> io::Result<bool> {
    let Some(path) = state_file_path() else {
        return Ok(false);
    };
    crate::ensure_parent(&path)?;
    crate::write_bytes_atomically(&path, serialize_event(event).as_bytes())?;
    Ok(true)
}

/// Read the last event only when it belongs to the workspace currently shown in the pane.
pub(crate) fn last_event_for(context: &PluginContext) -> Option<PluginEvent> {
    let workspace_id = context.workspace_id.as_deref()?;
    let event = read_last_event()?;
    event_matches_workspace(&event, workspace_id).then_some(event)
}

fn read_last_event() -> Option<PluginEvent> {
    let path = state_file_path()?;
    let file = fs::File::open(path).ok()?;
    let mut body = Vec::new();
    file.take(MAX_CONTEXT_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .ok()?;
    if body.len() > MAX_CONTEXT_BYTES {
        return None;
    }
    let body = String::from_utf8(body).ok()?;
    parse_stored_event(&body)
}

fn event_matches_workspace(event: &PluginEvent, workspace_id: &str) -> bool {
    event.context.workspace_id.as_deref() == Some(workspace_id)
}

fn state_file_path() -> Option<PathBuf> {
    env::var_os("HERDR_PLUGIN_STATE_DIR")
        .filter(|value| !value.is_empty())
        .map(|dir| PathBuf::from(dir).join(LAST_EVENT_FILE))
}

fn parse_stored_event(input: &str) -> Option<PluginEvent> {
    if input.len() > MAX_CONTEXT_BYTES {
        return None;
    }
    let value = json::parse(input).ok()?;
    let name = value.str_field("event")?.to_string();
    if !is_tracked_event(&name) {
        return None;
    }
    let context = context_from_value(&value, None);
    context.workspace_id.as_ref()?;
    Some(PluginEvent { name, context })
}

fn serialize_event(event: &PluginEvent) -> String {
    let c = &event.context;
    format!(
        "{{\"event\":{},\"workspace_id\":{},\"workspace_label\":{},\"workspace_cwd\":{},\"worktree_branch\":{},\"worktree_path\":{}}}",
        quote(&event.name),
        optional_quote(c.workspace_id.as_deref()),
        optional_quote(c.workspace_label.as_deref()),
        optional_quote(c.workspace_cwd.as_deref()),
        optional_quote(c.worktree_branch.as_deref()),
        optional_quote(c.worktree_path.as_deref()),
    )
}

fn optional_quote(value: Option<&str>) -> String {
    value.map(quote).unwrap_or_else(|| "null".to_string())
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn context_from_env_vars() -> PluginContext {
    PluginContext {
        workspace_id: env_value("HERDR_WORKSPACE_ID"),
        tab_id: env_value("HERDR_TAB_ID"),
        focused_pane_id: env_value("HERDR_PANE_ID"),
        ..Default::default()
    }
}

fn env_value(name: &str) -> Option<String> {
    env::var(name).ok().and_then(|value| clean_string(&value))
}

fn parse_object(input: &str, label: &str) -> Result<Value, String> {
    if input.len() > MAX_CONTEXT_BYTES {
        return Err(format!("{} exceeds {} bytes", label, MAX_CONTEXT_BYTES));
    }
    let value =
        json::parse(input).map_err(|error| format!("could not parse {}: {}", label, error))?;
    if !matches!(value, Value::Obj(_)) {
        return Err(format!("{} must be a JSON object", label));
    }
    Ok(value)
}

fn context_from_value(root: &Value, data: Option<&Value>) -> PluginContext {
    let workspace = data
        .and_then(|value| value.get("workspace"))
        .or_else(|| root.get("workspace"));
    let tab = data
        .and_then(|value| value.get("tab"))
        .or_else(|| root.get("tab"));
    let pane = data
        .and_then(|value| value.get("focused_pane"))
        .or_else(|| root.get("focused_pane"))
        .or_else(|| data.and_then(|value| value.get("pane")))
        .or_else(|| root.get("pane"));
    let worktree = data
        .and_then(|value| value.get("worktree"))
        .or_else(|| workspace.and_then(|value| value.get("worktree")))
        .or_else(|| root.get("worktree"));

    let mut flat_sources = vec![root];
    if let Some(value) = data {
        flat_sources.insert(0, value);
    }
    let workspace_sources = workspace.into_iter().collect::<Vec<_>>();
    let tab_sources = tab.into_iter().collect::<Vec<_>>();
    let pane_sources = pane.into_iter().collect::<Vec<_>>();
    let worktree_sources = worktree.into_iter().collect::<Vec<_>>();

    PluginContext {
        workspace_id: first_field(&flat_sources, &["workspace_id"])
            .or_else(|| first_field(&workspace_sources, &["workspace_id", "id"])),
        workspace_label: first_field(&flat_sources, &["workspace_label"])
            .or_else(|| first_field(&workspace_sources, &["workspace_label", "label", "name"])),
        workspace_cwd: first_field(&flat_sources, &["workspace_cwd"])
            .or_else(|| first_field(&workspace_sources, &["workspace_cwd", "cwd", "path"])),
        tab_id: first_field(&flat_sources, &["tab_id"])
            .or_else(|| first_field(&workspace_sources, &["active_tab_id"]))
            .or_else(|| first_field(&tab_sources, &["tab_id", "id"])),
        tab_label: first_field(&flat_sources, &["tab_label"])
            .or_else(|| first_field(&tab_sources, &["tab_label", "label", "name"])),
        focused_pane_id: first_field(&flat_sources, &["focused_pane_id", "pane_id"])
            .or_else(|| first_field(&pane_sources, &["focused_pane_id", "pane_id", "id"])),
        focused_pane_cwd: first_field(&flat_sources, &["focused_pane_cwd"])
            .or_else(|| first_field(&pane_sources, &["focused_pane_cwd", "cwd", "path"])),
        focused_pane_agent: first_field(&flat_sources, &["focused_pane_agent"])
            .or_else(|| first_field(&pane_sources, &["focused_pane_agent", "agent"])),
        focused_pane_status: first_field(&flat_sources, &["focused_pane_status"]).or_else(|| {
            first_field(
                &pane_sources,
                &["focused_pane_status", "agent_status", "status"],
            )
        }),
        worktree_branch: first_field(&flat_sources, &["worktree_branch", "branch"])
            .or_else(|| first_field(&worktree_sources, &["worktree_branch", "branch"])),
        worktree_path: first_field(&flat_sources, &["worktree_path", "checkout_path"]).or_else(
            || {
                first_field(
                    &worktree_sources,
                    &["worktree_path", "checkout_path", "path"],
                )
            },
        ),
    }
}

fn first_field(sources: &[&Value], keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        sources
            .iter()
            .find_map(|source| source.str_field(key).and_then(clean_string))
    })
}

fn clean_string(value: &str) -> Option<String> {
    let cleaned: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_FIELD_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let keep: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_flat_context_shape_from_herdr() {
        let context = parse_context_json(
            r#"{
                "workspace_id":"w1",
                "workspace_label":"herdr-lazy",
                "workspace_cwd":"/repo/herdr-lazy",
                "tab_id":"w1:t1",
                "tab_label":"dev",
                "focused_pane_id":"w1:p1",
                "focused_pane_cwd":"/repo/herdr-lazy",
                "focused_pane_agent":"codex",
                "focused_pane_status":"working",
                "worktree":{"branch":"feature/context","path":"/repo/worktree"}
            }"#,
        )
        .unwrap();

        assert_eq!(context.workspace_id.as_deref(), Some("w1"));
        assert_eq!(context.workspace_label.as_deref(), Some("herdr-lazy"));
        assert_eq!(context.workspace_cwd.as_deref(), Some("/repo/herdr-lazy"));
        assert_eq!(context.tab_id.as_deref(), Some("w1:t1"));
        assert_eq!(context.focused_pane_agent.as_deref(), Some("codex"));
        assert_eq!(context.worktree_branch.as_deref(), Some("feature/context"));
        assert_eq!(context.worktree_path.as_deref(), Some("/repo/worktree"));
        assert_eq!(
            context.summary().as_deref(),
            Some("workspace herdr-lazy (w1) · cwd /repo/herdr-lazy · branch feature/context · agent codex (working)")
        );
    }

    #[test]
    fn parses_event_payload_context_and_prefers_runtime_ids() {
        let runtime = PluginContext {
            workspace_id: Some("env-workspace".to_string()),
            focused_pane_id: Some("env-pane".to_string()),
            ..Default::default()
        };
        let event = parse_event(
            "worktree.opened",
            Some(
                r#"{"data":{"workspace":{"workspace_id":"payload-workspace","active_tab_id":"w:t","worktree":{"checkout_path":"/repo/wt"}},"worktree":{"branch":"main","path":"/repo/wt"}}}"#,
            ),
            &runtime,
        )
        .unwrap()
        .unwrap();

        assert_eq!(event.name, "worktree.opened");
        assert_eq!(event.context.workspace_id.as_deref(), Some("env-workspace"));
        assert_eq!(event.context.focused_pane_id.as_deref(), Some("env-pane"));
        assert_eq!(event.context.tab_id.as_deref(), Some("w:t"));
        assert_eq!(event.context.worktree_branch.as_deref(), Some("main"));
        assert_eq!(event.context.worktree_path.as_deref(), Some("/repo/wt"));
    }

    #[test]
    fn unknown_or_contextless_events_are_ignored() {
        let context = PluginContext::default();
        assert!(parse_event("workspace.focused", None, &context)
            .unwrap()
            .is_none());
        assert!(parse_event("worktree.created", None, &context)
            .unwrap()
            .is_none());
    }

    #[test]
    fn malformed_payload_is_reported_without_panicking() {
        let error = parse_event(
            "worktree.created",
            Some("{not-json"),
            &PluginContext {
                workspace_id: Some("w1".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("HERDR_PLUGIN_EVENT_JSON"));
    }

    #[test]
    fn control_characters_are_removed_before_they_reach_the_ui() {
        let context =
            parse_context_json(r#"{"workspace_id":"w1","workspace_label":"safe\u001b[31m"}"#)
                .unwrap();
        assert_eq!(context.workspace_label.as_deref(), Some("safe[31m"));
    }

    #[test]
    fn stored_event_round_trips_through_the_minimal_json_writer() {
        let event = PluginEvent {
            name: "worktree.created".to_string(),
            context: PluginContext {
                workspace_id: Some("w1".to_string()),
                workspace_label: Some("a \"workspace\"".to_string()),
                workspace_cwd: Some("/tmp/worktree".to_string()),
                worktree_branch: Some("feature/context".to_string()),
                ..Default::default()
            },
        };
        let parsed = parse_stored_event(&serialize_event(&event)).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn oversized_stored_events_are_ignored() {
        let oversized = format!(
            "{{\"event\":\"worktree.opened\",\"workspace_id\":\"{}\"}}",
            "x".repeat(MAX_CONTEXT_BYTES)
        );
        assert!(parse_stored_event(&oversized).is_none());
    }

    #[test]
    fn an_event_hint_never_leaks_into_another_workspace() {
        let event = PluginEvent {
            name: "worktree.opened".to_string(),
            context: PluginContext {
                workspace_id: Some("w1".to_string()),
                ..Default::default()
            },
        };
        assert!(event_matches_workspace(&event, "w1"));
        assert!(!event_matches_workspace(&event, "w2"));
    }
}
