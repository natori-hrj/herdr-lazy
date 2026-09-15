//! Read-only machine health and explicitly targeted plugin actions.
//!
//! Herdr's saved-machine view is deliberately treated as an optional capability. Older Herdr
//! clients know about `machine list` but do not expose the global `--machine` dispatcher, so
//! this module feature-detects that flag before asking for any remote state. A missing or
//! disconnected machine is data for the view, never a reason to change the local installation.

use std::io;

use crate::{json, HerdrVersion, Installed, PLUGIN_ID};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineProfile {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) target: String,
    pub(crate) session: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MachineConnection {
    Local,
    Connected,
    Disconnected,
    Incompatible,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncResult {
    pub(crate) status: String,
    pub(crate) detail: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MachineHealth {
    /// `None` identifies the local machine; saved SSH machines carry their profile here.
    pub(crate) profile: Option<MachineProfile>,
    pub(crate) connection: MachineConnection,
    pub(crate) herdr_version: Option<HerdrVersion>,
    pub(crate) compatible: Option<bool>,
    pub(crate) installed: Vec<Installed>,
    pub(crate) plugin_list_available: bool,
    pub(crate) action_ids: Vec<String>,
    pub(crate) last_sync: Option<SyncResult>,
    /// A warning or unavailable-state explanation. It is display-only; it never drives a
    /// destructive action.
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct MachineCollection {
    pub(crate) machines: Vec<MachineHealth>,
    pub(crate) notice: Option<String>,
}

/// The global dispatcher was added after the original saved-machine commands. Looking for the
/// advertised option avoids sending a command that an older client will reject, and gives the
/// manage pane a safe local-only fallback.
pub(crate) fn machine_forwarding_supported() -> bool {
    match crate::run_herdr(&["--help"]) {
        Ok((true, stdout, stderr)) => help_advertises_machine(&format!("{}\n{}", stdout, stderr)),
        _ => false,
    }
}

fn help_advertises_machine(help: &str) -> bool {
    help.split_whitespace().any(|word| {
        word == "--machine"
            || word
                .strip_prefix("--machine=")
                .is_some_and(|value| !value.is_empty())
    })
}

pub(crate) fn machine_profiles() -> Result<Vec<MachineProfile>, String> {
    match crate::run_herdr(&["machine", "list", "--json"]) {
        Ok((true, stdout, _)) => parse_machine_list(&stdout),
        Ok((false, stdout, stderr)) => Err(command_error(
            "could not list saved machines",
            &stdout,
            &stderr,
        )),
        Err(error) => Err(format!("could not run herdr: {}", error)),
    }
}

fn parse_machine_list(stdout: &str) -> Result<Vec<MachineProfile>, String> {
    let value =
        json::parse(stdout.trim()).map_err(|e| format!("could not parse machine JSON: {}", e))?;
    let machines = value
        .as_array()
        .ok_or("machine list JSON was not an array")?;

    machines
        .iter()
        .enumerate()
        .map(|(index, machine)| {
            let id = required_machine_field(machine, "id", index)?;
            if !valid_machine_id(&id) {
                return Err(format!("machine {} has an unsafe id", index + 1));
            }
            let enabled = machine
                .get("enabled")
                .and_then(|value| value.as_bool())
                .ok_or_else(|| format!("machine {} has no boolean `enabled` field", index + 1))?;
            Ok(MachineProfile {
                id,
                label: required_machine_field(machine, "label", index)?,
                target: required_machine_field(machine, "target", index)?,
                session: required_machine_field(machine, "session", index)?,
                enabled,
            })
        })
        .collect()
}

fn required_machine_field(
    value: &json::Value,
    field: &str,
    index: usize,
) -> Result<String, String> {
    let raw = value
        .str_field(field)
        .ok_or_else(|| format!("machine {} has no string `{}` field", index + 1, field))?;
    let cleaned = one_line(raw);
    if cleaned.is_empty() {
        Err(format!(
            "machine {} has an empty `{}` field",
            index + 1,
            field
        ))
    } else {
        Ok(cleaned)
    }
}

fn valid_machine_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, '-' | '_'))
}

fn valid_cli_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// Make Herdr-owned labels safe for a one-line terminal view. The profile remains identifiable;
/// terminal controls and line breaks never reach the renderer.
fn one_line(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn run_machine(profile_id: &str, args: &[&str]) -> io::Result<(bool, String, String)> {
    if !valid_machine_id(profile_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe machine id",
        ));
    }
    let mut owned = vec!["--machine".to_string(), profile_id.to_string()];
    owned.extend(args.iter().map(|arg| (*arg).to_string()));
    crate::run_herdr_owned(&owned)
}

fn remote_server_status(profile_id: &str) -> Result<ServerStatus, String> {
    match run_machine(profile_id, &["status", "server", "--json"]) {
        Ok((true, stdout, _)) => parse_server_status(&stdout),
        Ok((false, stdout, stderr)) => {
            Err(command_error("server status unavailable", &stdout, &stderr))
        }
        Err(error) => Err(format!("could not run herdr: {}", error)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerStatus {
    running: bool,
    version: Option<HerdrVersion>,
    compatible: Option<bool>,
}

fn parse_server_status(stdout: &str) -> Result<ServerStatus, String> {
    let value =
        json::parse(stdout.trim()).map_err(|e| format!("could not parse server status: {}", e))?;
    let object = [
        value.path(&["result", "server"]),
        value.path(&["server"]),
        value.path(&["result"]),
        Some(&value),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| candidate.get("status").is_some() || candidate.get("running").is_some())
    .ok_or("server status had no status fields")?;

    let status = object.str_field("status");
    let running = object
        .get("running")
        .and_then(|value| value.as_bool())
        .or_else(|| status.map(|value| value == "running"))
        .ok_or("server status had no boolean running field")?;
    let version = object.str_field("version").and_then(HerdrVersion::parse);
    let endpoint_compatible = object
        .get("endpoint_compatible")
        .and_then(|value| value.as_bool());
    let server_compatible = object.get("compatible").and_then(|value| value.as_bool());
    let compatible = match (endpoint_compatible, server_compatible) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), _) | (_, Some(true)) => Some(true),
        _ => None,
    };

    Ok(ServerStatus {
        running,
        version,
        compatible,
    })
}

fn remote_plugin_list(profile_id: &str) -> Result<Vec<Installed>, String> {
    match run_machine(profile_id, &["plugin", "list", "--json"]) {
        Ok((true, stdout, _)) => crate::parse_plugin_list(&stdout),
        Ok((false, stdout, stderr)) => {
            Err(command_error("plugin list unavailable", &stdout, &stderr))
        }
        Err(error) => Err(format!("could not run herdr: {}", error)),
    }
}

fn remote_action_ids(profile_id: &str) -> Result<Vec<String>, String> {
    match run_machine(
        profile_id,
        &["plugin", "action", "list", "--plugin", PLUGIN_ID],
    ) {
        Ok((true, stdout, _)) => parse_action_list(&stdout),
        Ok((false, stdout, stderr)) => Err(command_error(
            "plugin actions unavailable",
            &stdout,
            &stderr,
        )),
        Err(error) => Err(format!("could not run herdr: {}", error)),
    }
}

fn parse_action_list(stdout: &str) -> Result<Vec<String>, String> {
    let value =
        json::parse(stdout.trim()).map_err(|e| format!("could not parse action JSON: {}", e))?;
    let actions = value
        .path(&["result", "actions"])
        .or_else(|| value.path(&["actions"]))
        .and_then(|value| value.as_array())
        .ok_or("action list JSON had no actions array")?;
    Ok(actions
        .iter()
        .filter_map(|action| {
            action
                .str_field("action_id")
                .or_else(|| action.str_field("id"))
                .filter(|id| valid_cli_value(id))
                .map(str::to_string)
        })
        .collect())
}

/// Pick the target platform's action when Herdr returns a platform-specific suffix. An exact
/// base id wins, which keeps Unix manifests and the existing action names unchanged.
pub(crate) fn action_for(actions: &[String], base: &str) -> Option<String> {
    actions
        .iter()
        .find(|id| id.as_str() == base)
        .cloned()
        .or_else(|| {
            actions
                .iter()
                .find(|id| {
                    id.strip_prefix(base)
                        .is_some_and(|rest| rest.starts_with('-'))
                })
                .cloned()
        })
}

fn remote_sync_result(profile_id: &str) -> Option<SyncResult> {
    let output = run_machine(
        profile_id,
        &[
            "plugin", "log", "list", "--plugin", PLUGIN_ID, "--limit", "100",
        ],
    )
    .ok()
    .and_then(|(ok, stdout, _)| ok.then_some(stdout))?;
    parse_latest_sync_result(&output)
}

pub(crate) fn parse_latest_sync_result(stdout: &str) -> Option<SyncResult> {
    let value = json::parse(stdout.trim()).ok()?;
    let logs = value
        .path(&["result", "logs"])
        .or_else(|| value.path(&["logs"]))
        .and_then(|value| value.as_array())?;

    logs.iter()
        .filter_map(|log| {
            let action_id = log.str_field("action_id");
            let command_has_sync = log
                .get("command")
                .and_then(|value| value.as_array())
                .is_some_and(|command| command.iter().any(|part| part.as_str() == Some("sync")));
            if !action_id.is_some_and(|id| id == "sync" || id.starts_with("sync-"))
                && !command_has_sync
            {
                return None;
            }
            let started = log.get("started_unix_ms").and_then(as_u64).unwrap_or(0);
            let status = log.str_field("status")?.to_string();
            let detail = if status == "failed" {
                ["error", "stderr", "stdout"]
                    .into_iter()
                    .find_map(|field| log.str_field(field).map(one_line))
                    .filter(|detail| !detail.is_empty())
            } else {
                None
            };
            Some((started, SyncResult { status, detail }))
        })
        .max_by_key(|(started, _)| *started)
        .map(|(_, result)| result)
}

fn as_u64(value: &json::Value) -> Option<u64> {
    match value {
        json::Value::Num(number) if number.is_finite() && *number >= 0.0 => Some(*number as u64),
        _ => None,
    }
}

fn command_error(prefix: &str, stdout: &str, stderr: &str) -> String {
    let detail = if !stderr.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    if detail.is_empty() {
        prefix.to_string()
    } else {
        format!("{}: {}", prefix, one_line(&truncate(detail, 240)))
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        let keep: String = value.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", keep)
    }
}

pub(crate) fn invoke_machine_action(profile_id: &str, action_id: &str) -> Result<String, String> {
    if !valid_cli_value(action_id) {
        return Err("unsafe plugin action id".to_string());
    }
    match run_machine(
        profile_id,
        &[
            "plugin", "action", "invoke", action_id, "--plugin", PLUGIN_ID,
        ],
    ) {
        Ok((true, stdout, _)) => Ok(stdout),
        Ok((false, stdout, stderr)) => {
            Err(command_error("machine action failed", &stdout, &stderr))
        }
        Err(error) => Err(format!("could not run herdr: {}", error)),
    }
}

pub(crate) fn collect_machine_health(
    local_installed: &[Installed],
    local_herdr_version: Option<HerdrVersion>,
) -> MachineCollection {
    let local_actions = local_installed
        .iter()
        .find(|plugin| plugin.plugin_id == PLUGIN_ID)
        .map(|plugin| plugin.actions.iter().map(|(id, _)| id.clone()).collect())
        .unwrap_or_default();
    let local_sync = crate::plugin_logs(PLUGIN_ID)
        .ok()
        .and_then(|output| parse_latest_sync_result(&output));
    let local = MachineHealth {
        profile: None,
        connection: MachineConnection::Local,
        herdr_version: local_herdr_version,
        compatible: Some(true),
        installed: local_installed.to_vec(),
        plugin_list_available: true,
        action_ids: local_actions,
        last_sync: local_sync,
        error: None,
    };

    if !machine_forwarding_supported() {
        return MachineCollection {
            machines: vec![local],
            notice: Some(
                "saved-machine status is unavailable in this Herdr CLI — showing local only"
                    .to_string(),
            ),
        };
    }

    let profiles = match machine_profiles() {
        Ok(profiles) => profiles,
        Err(error) => {
            return MachineCollection {
                machines: vec![local],
                notice: Some(format!(
                    "saved-machine list unavailable — local only: {}",
                    error
                )),
            }
        }
    };

    let mut machines = vec![local];
    for profile in profiles {
        machines.push(collect_saved_machine(profile));
    }
    MachineCollection {
        machines,
        notice: None,
    }
}

fn collect_saved_machine(profile: MachineProfile) -> MachineHealth {
    if !profile.enabled {
        return MachineHealth {
            profile: Some(profile),
            connection: MachineConnection::Disabled,
            herdr_version: None,
            compatible: None,
            installed: Vec::new(),
            plugin_list_available: false,
            action_ids: Vec::new(),
            last_sync: None,
            error: Some("saved machine is disabled".to_string()),
        };
    }

    let server = remote_server_status(&profile.id);
    let plugin_list = remote_plugin_list(&profile.id);
    let (herdr_version, compatible, server_running, server_error) = match server {
        Ok(status) => (
            status.version,
            status.compatible,
            Some(status.running),
            (!status.running).then_some("Herdr server is not running".to_string()),
        ),
        Err(error) => (None, None, None, Some(error)),
    };

    let (installed, plugin_list_available, plugin_error) = match plugin_list {
        Ok(installed) => (installed, true, None),
        Err(error) => (Vec::new(), false, Some(error)),
    };
    let (action_ids, action_error, last_sync) = if plugin_list_available {
        let action_result = remote_action_ids(&profile.id);
        (
            action_result.clone().unwrap_or_default(),
            action_result.err(),
            remote_sync_result(&profile.id),
        )
    } else {
        (Vec::new(), None, None)
    };

    let connection = if !plugin_list_available || server_running == Some(false) {
        MachineConnection::Disconnected
    } else if compatible == Some(false) {
        MachineConnection::Incompatible
    } else {
        MachineConnection::Connected
    };
    let error = [server_error, plugin_error, action_error]
        .into_iter()
        .flatten()
        .map(|error| one_line(&error))
        .filter(|error| !error.is_empty())
        .collect::<Vec<_>>();

    MachineHealth {
        profile: Some(profile),
        connection,
        herdr_version,
        compatible,
        installed,
        plugin_list_available,
        action_ids,
        last_sync,
        error: (!error.is_empty()).then(|| error.join(" · ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_list_matches_herdrs_bare_array_shape() {
        let machines = parse_machine_list(
            r#"[
              {"id":"0123456789abcdef0123456789abcdef","label":"Build machine","target":"dev@example.com","session":"default","enabled":true,"selected":false},
              {"id":"abcdefabcdefabcdefabcdefabcdefab","label":"Disabled","target":"dev@old","session":"ci","enabled":false,"selected":false}
            ]"#,
        )
        .expect("machine list should parse");

        assert_eq!(machines.len(), 2);
        assert_eq!(machines[0].label, "Build machine");
        assert!(machines[0].enabled);
        assert!(!machines[1].enabled);
    }

    #[test]
    fn machine_list_rejects_unsafe_ids() {
        let error = parse_machine_list(
            r#"[{"id":"build machine","label":"Build","target":"dev@host","session":"default","enabled":true}]"#,
        )
        .expect_err("an id is passed to a command and must be validated");
        assert!(error.contains("unsafe id"));
    }

    #[test]
    fn help_detection_requires_the_machine_option() {
        assert!(help_advertises_machine(
            "Options: --session <name> --machine <label-or-id>"
        ));
        assert!(!help_advertises_machine("Commands: machine list --json"));
    }

    #[test]
    fn server_status_accepts_running_and_compatibility_fields() {
        let status = parse_server_status(
            r#"{"result":{"status":"running","running":true,"version":"herdr 0.9.0","endpoint_compatible":true}}"#,
        )
        .expect("status should parse");
        assert!(status.running);
        assert_eq!(status.version, HerdrVersion::parse("0.9.0"));
        assert_eq!(status.compatible, Some(true));
    }

    #[test]
    fn action_list_accepts_action_id_and_legacy_id() {
        let ids = parse_action_list(
            r#"{"result":{"actions":[{"action_id":"sync","title":"Sync","command":[]},{"id":"update-windows","title":"Update","command":[]}]}}"#,
        )
        .expect("actions should parse");
        assert_eq!(ids, ["sync", "update-windows"]);
        assert_eq!(
            action_for(&ids, "update"),
            Some("update-windows".to_string())
        );
    }

    #[test]
    fn latest_sync_result_uses_the_newest_sync_log() {
        let result = parse_latest_sync_result(
            r#"{"result":{"logs":[
              {"action_id":"sync","status":"succeeded","started_unix_ms":10},
              {"action_id":"sync","status":"failed","started_unix_ms":20,"error":"remote refused"},
              {"action_id":"manage","status":"succeeded","started_unix_ms":30}
            ]}}"#,
        )
        .expect("a sync result should be found");
        assert_eq!(result.status, "failed");
        assert_eq!(result.detail.as_deref(), Some("remote refused"));
    }

    #[test]
    fn machine_action_arguments_keep_the_target_before_the_subcommand() {
        let mut args = vec![
            "--machine".to_string(),
            "0123456789abcdef0123456789abcdef".to_string(),
        ];
        args.extend(
            ["plugin", "action", "invoke", "sync", "--plugin", PLUGIN_ID].map(str::to_string),
        );
        assert_eq!(
            args,
            [
                "--machine",
                "0123456789abcdef0123456789abcdef",
                "plugin",
                "action",
                "invoke",
                "sync",
                "--plugin",
                "herdr-lazy"
            ]
        );
    }
}
