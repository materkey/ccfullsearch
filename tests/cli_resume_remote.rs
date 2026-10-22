use std::os::unix::fs::PermissionsExt;
use std::process::Command;

/// A fake ssh that records its argv, one argument per line, and exits 0.
fn recording_ssh(dir: &tempfile::TempDir) -> (String, std::path::PathBuf) {
    let log = dir.path().join("argv.log");
    let script = dir.path().join("ssh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script.to_string_lossy().to_string(), log)
}

#[test]
fn resume_host_when_fake_ssh_then_remote_command_has_quoted_file_and_uuid() {
    let dir = tempfile::TempDir::new().unwrap();
    let (ssh, log) = recording_ssh(&dir);
    let status = Command::new(env!("CARGO_BIN_EXE_ccs"))
        .args([
            "resume",
            "--host",
            "fake",
            "--session-id",
            "ses_1",
            "--file",
            "/a b/s.jsonl",
            "--uuid",
            "u1",
        ])
        .env("CCS_SSH", &ssh)
        .env("CCS_HOSTS", "fake")
        .env("XDG_CONFIG_HOME", dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    let argv = std::fs::read_to_string(log).unwrap();
    let args: Vec<&str> = argv.lines().collect();
    assert_eq!(args.first(), Some(&"-t"));
    assert_eq!(
        args.last(),
        Some(&"ccs resume --session-id 'ses_1' --file '/a b/s.jsonl' --uuid 'u1'")
    );
}

#[test]
fn resume_host_when_host_unknown_then_exit_1() {
    let out = Command::new(env!("CARGO_BIN_EXE_ccs"))
        .args(["resume", "--host", "nosuch", "--file", "/s.jsonl"])
        .env("CCS_HOSTS", "")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nosuch"));
}

#[test]
fn resume_here_when_agent_missing_then_exit_1_without_prompt() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ccs"))
        .args(["resume", "--file", "/nonexistent/sess.jsonl"])
        .env("PATH", dir.path())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("Press Enter"), "{stderr}");
}
