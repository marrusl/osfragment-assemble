use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn no_args_shows_help_or_requires_manifest() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.assert().failure();
}

#[test]
fn inspect_local_directory() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/epel"])
        .assert()
        .success()
        .stdout(predicate::str::contains("epel"))
        .stdout(predicate::str::contains("repos"))
        .stdout(predicate::str::contains("yum.repos.d"));
}

#[test]
fn inspect_tailscale_shows_script() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/tailscale"])
        .assert()
        .success()
        .stdout(predicate::str::contains("configure.sh"));
}

#[test]
fn list_with_local_manifest() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args([
        "list",
        "--manifest",
        "examples/manifests/minimal.yaml",
        "--local",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("epel"))
    .stdout(predicate::str::contains("cis-hardening"));
}

// Requires podman (prebuild + local digest) and skopeo with RHEL
// registry credentials (base image digest resolution). Run with:
//   cargo test -- --ignored
#[test]
#[ignore]
fn assemble_with_local_fragments() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args([
        "--manifest",
        "examples/manifests/minimal.yaml",
        "--local",
        "--output",
        "/tmp/bootc-assemble-test-containerfile",
    ])
    .assert()
    .success();

    let content = std::fs::read_to_string("/tmp/bootc-assemble-test-containerfile").unwrap();
    assert!(content.contains("Phase: repos"));
    assert!(content.contains("Phase: packages"));
    assert!(content.contains("bootc container lint"));
    std::fs::remove_file("/tmp/bootc-assemble-test-containerfile").ok();
}
