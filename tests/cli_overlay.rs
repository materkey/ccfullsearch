#![allow(deprecated)]
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn overlay_flag_appears_in_help() {
    Command::cargo_bin("ccs")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--overlay"));
}

#[test]
fn overlay_flag_description_in_help() {
    Command::cargo_bin("ccs")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Overlay mode"));
}
