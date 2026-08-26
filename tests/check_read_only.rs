#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const COMMIT: &str = "10e93033263549600e75119c5617dac48137d011";

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn fixture_root() -> PathBuf {
    std::env::temp_dir().join(format!("herdr-lazy-check-read-only-{}", std::process::id()))
}

#[test]
fn check_does_not_mutate_plugin_state_or_config_files() {
    let root = fixture_root();
    let bin = root.join("bin");
    let list = root.join("plugins.list");
    let lock = root.join("plugins.lock");
    let plugin_json = root.join("plugins.json");
    let market_json = root.join("market.json");
    let command_log = root.join("commands.log");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&bin).unwrap();

    let list_body = b"owner/current\n";
    let lock_body = b"# sentinel lock\nowner/current@sentinel\n";
    fs::write(&list, list_body).unwrap();
    fs::write(&lock, lock_body).unwrap();
    fs::write(
        &plugin_json,
        format!(
            r#"{{"result":{{"plugins":[{{"enabled":true,"name":"current","plugin_id":"current","source":{{"kind":"github","owner":"owner","repo":"current","installed_unix_ms":0,"resolved_commit":"{}"}}}}]}}}}"#,
            COMMIT
        ),
    )
    .unwrap();
    fs::write(
        &market_json,
        r#"{"plugins":[{"fullName":"owner/current","pushedAt":"1970-01-01T00:00:00Z"}]}"#,
    )
    .unwrap();

    write_executable(
        &bin.join("herdr"),
        r#"#!/bin/sh
if [ "$1" = "plugin" ] && [ "$2" = "list" ] && [ "$3" = "--json" ]; then
  /bin/cat "$HERDR_PLUGIN_JSON"
  exit 0
fi
printf '%s\n' "$*" >> "$HERDR_COMMAND_LOG"
exit 42
"#,
    );
    write_executable(
        &bin.join("curl"),
        r#"#!/bin/sh
/bin/cat "$HERDR_MARKET_JSON"
"#,
    );

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("check")
        .env("HERDR_LAZY_LIST", &list)
        .env("HERDR_BIN_PATH", bin.join("herdr"))
        .env("HERDR_PLUGIN_JSON", &plugin_json)
        .env("HERDR_MARKET_JSON", &market_json)
        .env("HERDR_COMMAND_LOG", &command_log)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("owner/current"));
    assert!(stdout.contains("installed and current"));
    assert!(stdout.contains("read-only"));
    assert_eq!(fs::read(&list).unwrap(), list_body);
    assert_eq!(fs::read(&lock).unwrap(), lock_body);
    assert_eq!(
        fs::read_to_string(&command_log).unwrap_or_default(),
        "",
        "check must not invoke a mutating herdr command"
    );

    fs::remove_dir_all(root).unwrap();
}
