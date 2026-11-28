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

fn lock_command(root: &Path, list: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"));
    command
        .env("HERDR_LAZY_LIST", list)
        .env("HERDR_BIN_PATH", root.join("herdr"));
    command
}

#[test]
fn lock_rejects_expanded_proposal_without_changing_any_generation() {
    use std::os::unix::fs::MetadataExt;

    let root = fixture_root("lock-expanded-proposal");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let list = root.join("plugins.list");
    fs::write(&list, "owner/repo/one@abcdef0\nowner/repo/two@abcdef1\n").unwrap();

    // Both inputs are in budget, but the one resolved ref matches both subdir specs.
    // Encoding it twice plus the repo names and header exceeds the 64 KiB lock limit.
    let resolved = "x".repeat(32 * 1024);
    fs::write(
        root.join("inventory.json"),
        format!(
            r#"{{"result":{{"plugins":[{{"source":{{"kind":"github","owner":"owner","repo":"repo","resolved_commit":"{resolved}"}}}}]}}}}"#
        ),
    )
    .unwrap();
    write_executable(
        &root.join("herdr"),
        "#!/bin/sh\ncat \"${0%/*}/inventory.json\"\n",
    );
    let leaves = [
        ("plugins.lock", "owner/active\n"),
        ("plugins.lock.1", "owner/first\n"),
        ("plugins.lock.2", "owner/second\n"),
        ("plugins.lock.3", "owner/third\n"),
    ];
    let before = leaves.map(|(name, contents)| {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        fs::metadata(path).unwrap().ino()
    });

    let output = lock_command(&root, &list).arg("lock").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "unexpected proposal diagnostics");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("plugin lock") && stderr.contains("exceeds 65536 bytes"),
        "unexpected stderr: {stderr}"
    );
    for ((name, contents), ino) in leaves.into_iter().zip(before) {
        let path = root.join(name);
        assert_eq!(fs::read_to_string(&path).unwrap(), contents);
        assert_eq!(fs::metadata(path).unwrap().ino(), ino);
    }
    assert_eq!(fs::read_dir(&root).unwrap().count(), 7);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cooperating_lock_invocations_keep_each_published_generation() {
    use std::process::Stdio;

    let root = fixture_root("concurrent-lock");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    write_executable(
        &root.join("herdr"),
        "#!/bin/sh\nprintf '%s' '{\"result\":{\"plugins\":[]}}'\n",
    );
    fs::write(root.join("plugins.lock"), "owner/original\n").unwrap();

    let expected = ["owner/one", "owner/two", "owner/three", "owner/four"];
    let mut children = Vec::new();
    for (index, body) in expected.iter().enumerate() {
        let list = root.join(format!("bundle-{index}.list"));
        fs::write(&list, format!("{body}\n")).unwrap();
        children.push(
            lock_command(&root, &list)
                .arg("lock")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut actual: Vec<_> = [
        "plugins.lock",
        "plugins.lock.1",
        "plugins.lock.2",
        "plugins.lock.3",
    ]
    .iter()
    .map(|name| {
        fs::read_to_string(root.join(name))
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_owned()
    })
    .collect();
    actual.sort();
    let mut expected = expected;
    expected.sort();
    assert_eq!(
        actual.iter().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lock_and_both_restore_commands_reject_linked_lock_leaves() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let root = fixture_root("linked-locks");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    write_executable(
        &root.join("herdr"),
        "#!/bin/sh\nprintf '%s' '{\"result\":{\"plugins\":[]}}'\n",
    );
    let list = root.join("plugins.list");
    let active = root.join("plugins.lock");
    let prev = root.join("plugins.lock.1");
    let other = root.join("external");
    fs::write(&list, "owner/plugin\n").unwrap();
    fs::write(&other, "owner/external\n").unwrap();

    for (leaf, target, hardlink) in [
        (&active, "plugins.lock.1", false),
        (&prev, "plugins.lock", false),
        (&prev, "external", false),
        (&active, "external", true),
    ] {
        if hardlink {
            fs::hard_link(root.join(target), leaf).unwrap();
        } else {
            symlink(target, leaf).unwrap();
        }
        let before = fs::symlink_metadata(leaf).unwrap();
        for args in [&["lock"][..], &["restore"], &["restore", "--previous"]] {
            let output = lock_command(&root, &list).args(args).output().unwrap();
            assert!(!output.status.success(), "unexpected success for {args:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("independent regular file"), "{stderr}");
            let after = fs::symlink_metadata(leaf).unwrap();
            assert_eq!((after.ino(), after.nlink()), (before.ino(), before.nlink()));
            if !hardlink {
                assert_eq!(fs::read_link(leaf).unwrap(), Path::new(target));
            }
            let absent = if leaf == &active { &prev } else { &active };
            assert_eq!(
                fs::symlink_metadata(absent).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
            assert_eq!(fs::read(&other).unwrap(), b"owner/external\n");
        }
        fs::remove_file(leaf).unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}
