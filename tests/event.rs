use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fixture_root() -> PathBuf {
    std::env::temp_dir().join(format!("herdr-lazy-event-{}", std::process::id()))
}

#[test]
fn event_writes_only_plugin_owned_state() {
    let root = fixture_root();
    let state_dir = root.join("state");
    let list = root.join("plugins.list");
    let lock = root.join("plugins.lock");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let list_body = b"owner/current\n";
    let lock_body = b"# sentinel lock\nowner/current@sentinel\n";
    fs::write(&list, list_body).unwrap();
    fs::write(&lock, lock_body).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("event")
        .env("HERDR_PLUGIN_EVENT", "worktree.opened")
        .env(
            "HERDR_PLUGIN_CONTEXT_JSON",
            r#"{"workspace_id":"w1","workspace_label":"demo","worktree":{"branch":"main"}}"#,
        )
        .env("HERDR_PLUGIN_STATE_DIR", &state_dir)
        .env("HERDR_LAZY_LIST", &list)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "event failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&list).unwrap(), list_body);
    assert_eq!(fs::read(&lock).unwrap(), lock_body);
    assert_eq!(
        fs::read_to_string(state_dir.join("last-workspace-event.json")).unwrap(),
        r#"{"event":"worktree.opened","workspace_id":"w1","workspace_label":"demo","workspace_cwd":null,"worktree_branch":"main","worktree_path":null}"#
    );

    fs::remove_dir_all(root).unwrap();
}
