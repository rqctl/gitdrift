use assert_cmd::Command;

/// The TUI cannot be driven without a pty, but we can prove the binary no
/// longer refuses to start it and that it fails cleanly on a non-terminal
/// stdout rather than panicking, hanging, or wrecking the terminal.
#[test]
fn tui_mode_is_wired_up_and_exits_cleanly_without_a_terminal() {
    let d = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("gitdrift")
        .unwrap()
        .arg(d.path())
        .timeout(std::time::Duration::from_secs(20))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("not implemented yet"),
        "TUI should be wired up: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(
        stderr.contains("not a terminal"),
        "expected a clean refusal, got: {stderr}"
    );
}
