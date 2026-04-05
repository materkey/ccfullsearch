#![allow(deprecated)]
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn pick_subcommand_appears_in_help() {
    Command::cargo_bin("ccs")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("pick"));
}

#[test]
fn pick_help_shows_output_flag() {
    Command::cargo_bin("ccs")
        .unwrap()
        .args(["pick", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--output"));
}

#[test]
fn pick_help_shows_query_argument() {
    Command::cargo_bin("ccs")
        .unwrap()
        .args(["pick", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("query"));
}
