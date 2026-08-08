use assert_cmd::Command;
use osfragment_assemble::mount::MOUNT_SECTION_NOTE;
use predicates::prelude::*;
use std::path::Path;

const MOUNT_FRAGMENT_TOML: &str = "[fragment]\nname = \"rhel-entitlement\"\n\
     version = \"1.0\"\ndescription = \"entitlement certificates\"\n";

/// A fragment directory carrying `fragment.toml` and, when `mount_files` is
/// non-empty, exactly those files under `mount/`. An empty slice still
/// creates `mount/` itself, which is the empty-mount notice's trigger.
fn mount_fragment_dir(mount_files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fragment.toml"), MOUNT_FRAGMENT_TOML).unwrap();
    std::fs::create_dir_all(dir.path().join("mount")).unwrap();
    for rel in mount_files {
        let path = dir.path().join("mount").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"material").unwrap();
    }
    dir
}

fn inspect(dir: &Path) -> assert_cmd::assert::Assert {
    Command::cargo_bin("osfragment-assemble")
        .unwrap()
        .args(["inspect", dir.to_str().unwrap()])
        .assert()
}

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
fn inspect_without_mounts_prints_no_mount_section() {
    // Every shipped example fragment is mountless, so the section must be
    // absent rather than rendered empty.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/epel"])
        .assert()
        .success()
        .stdout(predicate::str::contains("mount/").not());
}

/// The mount rendering and the mount diagnostics go to different streams on
/// purpose: the section is output a reader may pipe somewhere, the notice is
/// commentary about the fragment. Only a real process gives the two streams
/// separately, so this is the surface that can hold that contract, and the
/// negative half of each assertion is what makes it a routing test rather
/// than a content test.
#[test]
fn inspect_prints_the_mount_section_on_stdout_and_nothing_on_stderr() {
    let dir = mount_fragment_dir(&["etc/pki/entitlement/cert.pem"]);
    inspect(dir.path())
        .success()
        .stdout(predicate::str::contains("mount/"))
        .stdout(predicate::str::contains("/etc/pki/entitlement"))
        .stdout(predicate::str::contains(MOUNT_SECTION_NOTE))
        .stderr(predicate::str::contains("mount/").not())
        .stderr(predicate::str::contains(MOUNT_SECTION_NOTE).not());
}

#[test]
fn inspect_prints_the_empty_mount_notice_on_stderr_and_not_on_stdout() {
    let dir = mount_fragment_dir(&[]);
    inspect(dir.path())
        .success()
        .stderr(predicate::str::contains("notice:"))
        .stderr(predicate::str::contains("derives no build mounts"))
        .stdout(predicate::str::contains("notice:").not())
        .stdout(predicate::str::contains("derives no build mounts").not());
}

#[test]
fn inspect_tailscale_shows_hook() {
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/tailscale"])
        .assert()
        .success()
        .stdout(predicate::str::contains("entrypoint"));
}

#[test]
fn inspect_grafana_shows_declarative_account_and_state() {
    // The grafana fragment declares its service account and state
    // directories as sysusers.d/tmpfiles.d tree content rather than
    // relying on the RPM's %post. Inspect must list both files, and
    // succeeding at all means the fragment satisfies the hooks
    // entrypoint contract.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/grafana"])
        .assert()
        .success()
        .stdout(predicate::str::contains("usr/lib/sysusers.d/grafana.conf"))
        .stdout(predicate::str::contains("usr/lib/tmpfiles.d/grafana.conf"))
        .stdout(predicate::str::contains("entrypoint"));
}

#[test]
fn self_contained_conflicts_with_ocp() {
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", "out", "--ocp"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn self_contained_conflicts_with_output() {
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", "out", "--output", "Containerfile"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn self_contained_alone_does_not_conflict_with_outputs_default() {
    // --output has a default value, but relying on that default (never
    // passing --output explicitly) must not trip conflicts_with: only an
    // explicit --output alongside --self-contained is an error. This run
    // fails for an unrelated reason (no manifest file in the test's cwd),
    // which is exactly the point: it must not fail on an --output conflict.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", "out"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("reading manifest")
                .or(predicate::str::contains("was not generated by this tool")),
        );
}

#[test]
fn self_contained_refuses_existing_foreign_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "not ours").unwrap();

    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", dir.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("was not generated by this tool"));
}

#[test]
fn materialize_mounts_requires_self_contained() {
    // The flag only means anything for the output mode that has to copy
    // mount material onto disk, so passing it alone must not be accepted
    // and then silently ignored.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--materialize-mounts"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--self-contained"));
}

#[test]
fn materialize_mounts_is_accepted_alongside_self_contained() {
    // Fails for an unrelated reason (no manifest in the test's cwd), which
    // is the point: it must not fail on argument parsing.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", "out", "--materialize-mounts"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("reading manifest")
                .or(predicate::str::contains("was not generated by this tool")),
        );
}
