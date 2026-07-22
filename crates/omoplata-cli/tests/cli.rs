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

#[test]
fn hash_object_then_cat_object() {
    let dir = tempdir().unwrap();
    omo().arg("init").arg(dir.path()).assert().success();

    let file = dir.path().join("hello.txt");
    std::fs::write(&file, b"hello omoplata\n").unwrap();

    let out = omo()
        .args(["hash-object", "--repo"])
        .arg(dir.path())
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8(out.stdout).unwrap();
    let id = id.trim();
    assert!(id.starts_with("sha256:"), "unexpected id: {id}");

    omo()
        .args(["cat-object", "--repo"])
        .arg(dir.path())
        .arg(id)
        .assert()
        .success()
        .stdout("hello omoplata\n");
}

#[test]
fn cat_object_unknown_fails() {
    let dir = tempdir().unwrap();
    omo().arg("init").arg(dir.path()).assert().success();
    omo()
        .args(["cat-object", "--repo"])
        .arg(dir.path())
        .arg("sha256:0000000000000000000000000000000000000000000000000000000000000000")
        .assert()
        .failure();
}

#[test]
fn diff_shows_hunk() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.txt");
    let target = dir.path().join("target.txt");
    std::fs::write(&base, "a\nb\nc\n").unwrap();
    std::fs::write(&target, "a\nx\nc\n").unwrap();
    omo()
        .arg("diff")
        .arg(&base)
        .arg(&target)
        .assert()
        .success()
        .stdout(predicate::str::contains("@@"))
        .stdout(predicate::str::contains("-b"))
        .stdout(predicate::str::contains("+x"));
}

#[test]
fn merge_clean_disjoint_edits() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.txt");
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&base, "a\nb\nc\nd\n").unwrap();
    std::fs::write(&left, "A\nb\nc\nd\n").unwrap(); // edits line 1
    std::fs::write(&right, "a\nb\nc\nD\n").unwrap(); // edits line 4
    omo()
        .arg("merge")
        .arg(&base)
        .arg(&left)
        .arg(&right)
        .assert()
        .success()
        .stdout("A\nb\nc\nD\n");
}

#[test]
fn merge_conflict_exits_nonzero_with_markers() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.txt");
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&base, "a\nb\nc\n").unwrap();
    std::fs::write(&left, "a\nX\nc\n").unwrap();
    std::fs::write(&right, "a\nY\nc\n").unwrap();
    omo()
        .arg("merge")
        .arg(&base)
        .arg(&left)
        .arg(&right)
        .assert()
        .failure()
        .stdout(predicate::str::contains("<<<<<<< left"))
        .stdout(predicate::str::contains("======="))
        .stdout(predicate::str::contains(">>>>>>> right"))
        .stderr(predicate::str::contains("1 conflict(s)"));
}
