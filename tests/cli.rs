use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn no_args_shows_help_or_requires_manifest() {
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.assert().failure();
}

#[test]
fn inspect_local_directory() {
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/epel"])
        .assert()
        .success()
        .stdout(predicate::str::contains("epel"))
        .stdout(predicate::str::contains("repos"))
        .stdout(predicate::str::contains("yum.repos.d"));
}

#[test]
fn inspect_tailscale_shows_hook() {
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/tailscale"])
        .assert()
        .success()
        .stdout(predicate::str::contains("configure.sh"));
}
