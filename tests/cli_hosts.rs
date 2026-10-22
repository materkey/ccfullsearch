//! `ccs search` / `list` / `show` across hosts, with a fake ssh that runs
//! this build's `ccs serve` locally as the "remote" host.
use std::process::{Command, Output};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const FAKE_SSH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-ssh.sh");

fn config(dir: &tempfile::TempDir, ccs_path: &str) {
    let ccs = dir.path().join("ccs");
    std::fs::create_dir_all(&ccs).unwrap();
    std::fs::write(
        ccs.join("config.toml"),
        format!("[[hosts]]\nname = \"fake\"\nccs_path = \"{ccs_path}\"\n"),
    )
    .unwrap();
}

fn ccs(dir: &tempfile::TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ccs"))
        .args(args)
        .env("CCFS_SEARCH_PATH", FIXTURES)
        .env("CCS_HOSTS", "fake")
        .env("CCS_SSH", FAKE_SSH)
        .env("XDG_CONFIG_HOME", dir.path())
        .output()
        .unwrap()
}

fn json_lines(out: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn host_status<'a>(summary: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    summary["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {summary}"))
}

#[test]
fn search_when_fake_host_then_rows_have_host_field_and_summary_hosts() {
    let dir = tempfile::TempDir::new().unwrap();
    config(&dir, env!("CARGO_BIN_EXE_ccs"));
    let out = ccs(&dir, &["search", "sort a list", "--limit", "50"]);
    assert!(out.status.success());
    let lines = json_lines(&out);
    let (summary, rows) = lines.split_last().unwrap();
    assert!(rows.iter().any(|r| r["host"] == "local"));
    assert!(rows.iter().any(|r| r["host"] == "fake"));
    assert_eq!(host_status(summary, "local")["status"], "ok");
    assert_eq!(host_status(summary, "fake")["status"], "ok");
}

#[test]
fn search_when_host_local_then_no_remote_rows() {
    let dir = tempfile::TempDir::new().unwrap();
    config(&dir, env!("CARGO_BIN_EXE_ccs"));
    let out = ccs(&dir, &["search", "sort a list", "--host", "local"]);
    let lines = json_lines(&out);
    let (_, rows) = lines.split_last().unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r["host"] == "local"));
}

#[test]
fn search_when_host_down_then_summary_reports_down_and_local_rows_present() {
    let dir = tempfile::TempDir::new().unwrap();
    config(&dir, "/nonexistent/ccs");
    let out = ccs(&dir, &["search", "sort a list"]);
    assert!(out.status.success());
    let lines = json_lines(&out);
    let (summary, rows) = lines.split_last().unwrap();
    assert!(rows.iter().any(|r| r["host"] == "local"));
    let fake = host_status(summary, "fake");
    assert_eq!(fake["status"], "down");
    assert!(fake["error"].as_str().is_some());
}

#[test]
fn search_when_host_unknown_then_error_exit() {
    let dir = tempfile::TempDir::new().unwrap();
    config(&dir, env!("CARGO_BIN_EXE_ccs"));
    let out = ccs(&dir, &["search", "x", "--host", "nosuch"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nosuch"));
}

#[test]
fn list_when_fake_host_then_rows_have_host_field() {
    let dir = tempfile::TempDir::new().unwrap();
    config(&dir, env!("CARGO_BIN_EXE_ccs"));
    let out = ccs(&dir, &["list", "--limit", "100"]);
    assert!(out.status.success());
    let rows = json_lines(&out);
    assert!(rows.iter().any(|r| r["host"] == "local"));
    assert!(rows.iter().any(|r| r["host"] == "fake"));
}

#[test]
fn show_when_host_given_then_reads_through_remote() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::TempDir::new().unwrap();
    // The "remote" ccs is a script: it records its argv and answers with a
    // marker no local read could produce.
    let argv = dir.path().join("argv");
    let script = dir.path().join("remote-ccs");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho '{{\"type\":\"summary\",\"from\":\"remote\"}}'\n",
            argv.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    config(&dir, &script.to_string_lossy());
    let file = format!("{FIXTURES}/linear_session.jsonl");
    let out = ccs(&dir, &["show", &file, "--uuid", "u2", "--host", "fake"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = json_lines(&out);
    assert_eq!(
        lines,
        vec![serde_json::json!({"type": "summary", "from": "remote"})]
    );
    let args = std::fs::read_to_string(argv).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(&args[..4], ["show", file.as_str(), "--uuid", "u2"]);
}
