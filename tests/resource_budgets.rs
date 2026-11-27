#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("herdr-lazy-resource-{name}-{}", std::process::id()))
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn run_check(root: &Path, herdr_body: &str) -> std::process::Output {
    let bin = root.join("bin");
    let list = root.join("plugins.list");
    fs::create_dir_all(&bin).unwrap();
    fs::write(&list, "owner/plugin\n").unwrap();
    write_executable(&bin.join("herdr"), herdr_body);

    Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .args(["check", "--json"])
        .env("HERDR_LAZY_LIST", list)
        .env("HERDR_BIN_PATH", bin.join("herdr"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .output()
        .unwrap()
}

#[test]
fn check_rejects_oversized_herdr_json_without_losing_the_reason() {
    let root = fixture_root("oversized-herdr");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let output = run_check(&root, "#!/bin/sh\nhead -c 9000000 /dev/zero\n");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("stdout exceeded 8388608 bytes"),
        "unexpected stdout: {stdout}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn check_keeps_bounded_stderr_from_a_failed_herdr_response() {
    let root = fixture_root("herdr-diagnostic");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let output = run_check(
        &root,
        "#!/bin/sh\nprintf '%s\\n' 'fixture diagnostic from herdr' >&2\nexit 42\n",
    );

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fixture diagnostic from herdr"),
        "unexpected stdout: {stdout}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn list_rejects_an_oversized_local_plugin_list() {
    let root = fixture_root("oversized-list");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let list = root.join("plugins.list");
    fs::write(&list, vec![b'x'; 64 * 1024 + 1]).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("list")
        .env("HERDR_LAZY_LIST", &list)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("plugin list") && stderr.contains("exceeds 65536 bytes"),
        "unexpected stderr: {stderr}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lock_rejects_oversized_legacy_migration_before_creating_current_state() {
    let root = fixture_root("oversized-legacy");
    let _ = fs::remove_dir_all(&root);
    let legacy = root.join("home/.config/herdr-lazy/plugins.list");
    let current_dir = root.join("current");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::create_dir_all(&current_dir).unwrap();
    fs::write(&legacy, vec![b'x'; 64 * 1024 + 1]).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("lock")
        .env("HOME", root.join("home"))
        .env("HERDR_PLUGIN_CONFIG_DIR", &current_dir)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("legacy plugin list") && stderr.contains("exceeds 65536 bytes"),
        "unexpected stderr: {stderr}"
    );
    assert!(!current_dir.join("plugins.list").exists());
    assert!(!current_dir.join("plugins.lock").exists());
    assert_eq!(fs::metadata(&legacy).unwrap().len(), 64 * 1024 + 1);
    fs::remove_dir_all(root).unwrap();
}

fn assert_direct_edit_rejects_oversized_legacy(command: &str) {
    let root = fixture_root(&format!("oversized-legacy-{command}"));
    let _ = fs::remove_dir_all(&root);
    let legacy = root.join("home/.config/herdr-lazy/plugins.list");
    let current_dir = root.join("current");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::create_dir_all(&current_dir).unwrap();
    let original = vec![b'x'; 64 * 1024 + 1];
    fs::write(&legacy, &original).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .args([command, "owner/plugin"])
        .env("HOME", root.join("home"))
        .env("HERDR_PLUGIN_CONFIG_DIR", &current_dir)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("legacy plugin list") && stderr.contains("exceeds 65536 bytes"),
        "unexpected stderr for {command}: {stderr}"
    );
    assert!(!current_dir.join("plugins.list").exists());
    assert_eq!(fs::read(&legacy).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn add_rejects_oversized_legacy_before_changing_either_list() {
    assert_direct_edit_rejects_oversized_legacy("add");
}

#[test]
fn remove_rejects_oversized_legacy_before_changing_either_list() {
    assert_direct_edit_rejects_oversized_legacy("remove");
}

#[test]
fn lock_inventory_failure_preserves_lock_and_history() {
    let root = fixture_root("lock-inventory-failure");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("bin")).unwrap();
    let list = root.join("plugins.list");
    let lock = root.join("plugins.lock");
    let history = root.join("plugins.lock.1");
    fs::write(&list, "owner/plugin\n").unwrap();
    fs::write(&lock, "owner/plugin@known-good\n").unwrap();
    fs::write(&history, "owner/plugin@older-good\n").unwrap();
    write_executable(
        &root.join("bin/herdr"),
        "#!/bin/sh\nhead -c 9000000 /dev/zero\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("lock")
        .env("HERDR_LAZY_LIST", &list)
        .env("HERDR_BIN_PATH", root.join("bin/herdr"))
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("stdout exceeded 8388608 bytes"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&lock).unwrap(),
        "owner/plugin@known-good\n"
    );
    assert_eq!(
        fs::read_to_string(&history).unwrap(),
        "owner/plugin@older-good\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_final_inventory_failure_does_not_rotate_a_valid_lock() {
    let root = fixture_root("sync-final-inventory-failure");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("bin")).unwrap();
    let list = root.join("plugins.list");
    let lock = root.join("plugins.lock");
    let history = root.join("plugins.lock.1");
    let count = root.join("inventory-count");
    fs::write(&list, "owner/plugin\n").unwrap();
    fs::write(&lock, "owner/plugin@known-good\n").unwrap();
    fs::write(&history, "owner/plugin@older-good\n").unwrap();
    write_executable(
        &root.join("bin/herdr"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  'plugin list --json')\n    n=$(wc -c < '{}')\n    printf x >> '{}'\n    if [ \"$n\" -eq 0 ]; then printf '{{\"result\":{{\"plugins\":[]}}}}'; else head -c 9000000 /dev/zero; fi\n    ;;\n  *) exit 0 ;;\nesac\n",
            count.display(),
            count.display()
        ),
    );
    fs::write(&count, "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("sync")
        .env("HERDR_LAZY_LIST", &list)
        .env("HERDR_BIN_PATH", root.join("bin/herdr"))
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lock and history were left unchanged"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&lock).unwrap(),
        "owner/plugin@known-good\n"
    );
    assert_eq!(
        fs::read_to_string(&history).unwrap(),
        "owner/plugin@older-good\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_inventory_failure_does_not_rotate_a_valid_lock() {
    let root = fixture_root("update-inventory-failure");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("bin")).unwrap();
    let list = root.join("plugins.list");
    let lock = root.join("plugins.lock");
    let history = root.join("plugins.lock.1");
    let count = root.join("inventory-count");
    fs::write(&list, "owner/plugin\n").unwrap();
    fs::write(&lock, "owner/plugin@known-good\n").unwrap();
    fs::write(&history, "owner/plugin@older-good\n").unwrap();
    fs::write(&count, "").unwrap();
    write_executable(
        &root.join("bin/herdr"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  'plugin list --json')\n    n=$(wc -c < '{}')\n    printf x >> '{}'\n    if [ \"$n\" -eq 0 ]; then printf '{{\"result\":{{\"plugins\":[]}}}}'; else head -c 9000000 /dev/zero; fi\n    ;;\n  *) exit 0 ;;\nesac\n",
            count.display(),
            count.display()
        ),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("update")
        .env("HERDR_LAZY_LIST", &list)
        .env("HERDR_BIN_PATH", root.join("bin/herdr"))
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lock and history were left unchanged"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&lock).unwrap(),
        "owner/plugin@known-good\n"
    );
    assert_eq!(
        fs::read_to_string(&history).unwrap(),
        "owner/plugin@older-good\n"
    );
    fs::remove_dir_all(root).unwrap();
}
