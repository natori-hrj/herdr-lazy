use std::process::Command;

#[test]
fn version_flags_report_the_package_version_without_herdr() {
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
            .arg(flag)
            .env_clear()
            .env("PATH", "")
            .env("HERDR_BIN_PATH", "nonexistent-herdr-for-version-test")
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert_eq!(
            output.stdout,
            format!("herdr-lazy {}\n", env!("CARGO_PKG_VERSION")).as_bytes(),
            "{flag}"
        );
        assert!(output.stderr.is_empty(), "{flag}");
    }
}

#[test]
fn help_lists_both_version_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-lazy"))
        .arg("help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.lines().any(|line| line.contains("-V, --version")));
}
