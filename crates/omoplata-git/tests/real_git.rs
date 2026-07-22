//! Integration test against a real git repository, guarded on `git` being on
//! PATH. If git is absent, the test prints a note and passes (it never fails the
//! suite for a missing tool).

use std::path::Path;
use std::process::Command;

use omoplata_git::{import_repo, verify_repo, GitObject};
use omoplata_store::{Object, Repository};

/// Whether `git` is available on PATH.
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn real_git_repo_verifies_and_imports() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping real_git integration test");
        return;
    }

    let work = tempfile::tempdir().unwrap();
    let root = work.path();

    git(root, &["init", "-q"]);
    std::fs::write(root.join("hello.txt"), b"hello omoplata\n").unwrap();
    git(root, &["add", "hello.txt"]);
    git(
        root,
        &[
            "-c",
            "user.email=test@omoplata.dev",
            "-c",
            "user.name=Omoplata Test",
            "commit",
            "-q",
            "-m",
            "initial",
        ],
    );

    let git_dir = root.join(".git");

    // The round-trip gate passes over a real repo: at least a blob, a tree, and
    // a commit.
    let report = verify_repo(&git_dir).expect("gate passes");
    assert!(report.blobs >= 1, "expected >=1 blob, got {}", report.blobs);
    assert!(report.trees >= 1, "expected >=1 tree, got {}", report.trees);
    assert!(
        report.commits >= 1,
        "expected >=1 commit, got {}",
        report.commits
    );

    // Import into a fresh omoplata repo.
    let omo_dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(omo_dir.path()).unwrap();
    let import = import_repo(&git_dir, &repo).expect("import succeeds");
    assert!(import.blobs >= 1);
    assert!(import.trees >= 1);
    assert!(import.mapping_count() >= import.blobs + import.trees);

    // The imported blob's content matches the original file: find the git blob
    // for "hello omoplata\n" and read its omoplata mapping back out of the store.
    let want = b"hello omoplata\n".to_vec();
    let blob_oid = import
        .git_objects
        .iter()
        .find_map(|(oid, obj)| match obj {
            GitObject::Blob(b) if *b == want => Some(*oid),
            _ => None,
        })
        .expect("blob present among imported objects");
    let store_id = import.oid_map.get(&blob_oid).expect("blob mapped");
    match repo.read_object(store_id).unwrap() {
        Object::Blob(b) => assert_eq!(b.bytes(), want.as_slice()),
        other => panic!("expected blob, got {other:?}"),
    }
}
