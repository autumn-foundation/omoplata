use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn omo() -> Command {
    Command::cargo_bin("omo").unwrap()
}

#[test]
fn version_flag() {
    omo()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_lists_subcommands() {
    omo()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("status"));
}

#[test]
fn init_then_status() {
    let dir = tempdir().unwrap();
    omo()
        .arg("init")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Initialized empty omoplata repository",
        ));
    assert!(dir.path().join(".omoplata").is_dir());
    omo()
        .arg("status")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("On omoplata repository"));
}

#[test]
fn init_twice_fails() {
    let dir = tempdir().unwrap();
    omo().arg("init").arg(dir.path()).assert().success();
    omo()
        .arg("init")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn status_uninitialized() {
    let dir = tempdir().unwrap();
    omo()
        .arg("status")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("not an omoplata repository"));
}
