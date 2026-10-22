use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

fn fixtures() -> String {
    format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))
}

fn spawn_serve() -> (Child, ChildStdin, BufReader<std::process::ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ccs"))
        .args(["serve", "--stdio"])
        .env("CCFS_SEARCH_PATH", fixtures())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    (child, stdin, stdout)
}

fn read_json(reader: &mut impl BufRead) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad line {line:?}: {e}"))
}

#[test]
fn serve_when_started_then_first_line_is_hello_with_protocol_1() {
    let (mut child, stdin, mut out) = spawn_serve();
    let hello = read_json(&mut out);
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["protocol"], 1);
    drop(stdin);
    child.wait().unwrap();
}

#[test]
fn serve_when_search_request_then_response_with_same_id_and_groups() {
    let (mut child, mut stdin, mut out) = spawn_serve();
    read_json(&mut out);
    writeln!(
        stdin,
        r#"{{"id":5,"method":"search","params":{{"query":"sort a list","regex":false,"project":null,"full_content":false,"limit":null}}}}"#
    )
    .unwrap();
    let reply = read_json(&mut out);
    assert_eq!(reply["id"], 5);
    assert!(!reply["result"]["groups"].as_array().unwrap().is_empty());
    drop(stdin);
    child.wait().unwrap();
}

#[test]
fn serve_when_unknown_method_then_error_response() {
    let (mut child, mut stdin, mut out) = spawn_serve();
    read_json(&mut out);
    writeln!(stdin, r#"{{"id":1,"method":"nope","params":{{}}}}"#).unwrap();
    let reply = read_json(&mut out);
    assert_eq!(reply["id"], 1);
    assert!(reply["error"].as_str().unwrap().contains("nope"));
    drop(stdin);
    child.wait().unwrap();
}

#[test]
fn serve_when_stdin_closed_then_exits_zero() {
    let (mut child, stdin, mut out) = spawn_serve();
    read_json(&mut out);
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
