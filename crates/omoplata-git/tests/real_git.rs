//! Integration test against a real git repository, guarded on `git` being on
//! PATH. If git is absent, the test prints a note and passes (it never fails the
//! suite for a missing tool).

use std::path::Path;
use std::process::Command;

use omoplata_git::{
    decode, encode, export_matches_source, export_repo, import_repo, read_refs, verify_repo,
    walk_loose, GitObject,
};
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

/// Commit with signing explicitly disabled, so tests do not depend on a signing
/// key being configured in the environment (the raw-body codec handles signed
/// commits regardless; determinism here is what matters for the test).
fn git_commit(dir: &Path, message: &str) {
    git(
        dir,
        &[
            "-c",
            "user.email=test@omoplata.dev",
            "-c",
            "user.name=Omoplata Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

/// Build a 2-commit, 2-file repo (a parent edge and a subtree) and return its
/// `.git` dir plus the tempdir keeping it alive.
fn two_commit_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("a.txt"), b"first\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub").join("b.txt"), b"nested\n").unwrap();
    git(root, &["add", "-A"]);
    git_commit(root, "first commit");
    std::fs::write(root.join("a.txt"), b"first\nsecond\n").unwrap();
    git(root, &["add", "-A"]);
    git_commit(root, "second commit");
    let git_dir = root.join(".git");
    (work, git_dir)
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

#[test]
fn commit_and_tag_reencode_byte_identically() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping commit/tag byte-identity test");
        return;
    }
    let (work, git_dir) = two_commit_repo();
    // Annotate a tag so there is a tag object to round-trip too.
    git(
        work.path(),
        &[
            "-c",
            "user.email=test@omoplata.dev",
            "-c",
            "user.name=Omoplata Test",
            "-c",
            "tag.gpgsign=false",
            "tag",
            "-a",
            "v1",
            "-m",
            "release one",
        ],
    );

    let mut saw_commit = false;
    let mut saw_tag = false;
    // walk_loose already verifies each object's oid against its path and that it
    // re-encodes byte-identically; here we additionally assert re-encode equality
    // explicitly for every commit and tag object.
    for (oid, object) in walk_loose(&git_dir).unwrap() {
        let encoded = encode(&object);
        // encode(decode(bytes)) == bytes, byte-for-byte.
        assert_eq!(encode(&decode(&encoded).unwrap()), encoded);
        // And the oid recomputed from the re-encoding matches the on-disk oid.
        assert_eq!(omoplata_git::oid(&object), oid);
        match object {
            GitObject::Commit(_) => saw_commit = true,
            GitObject::Tag(_) => saw_tag = true,
            _ => {}
        }
    }
    assert!(saw_commit, "expected at least one commit object");
    assert!(saw_tag, "expected the annotated tag object");
}

#[test]
fn read_refs_returns_head_and_branch() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping read_refs test");
        return;
    }
    let (_work, git_dir) = two_commit_repo();
    let refs = read_refs(&git_dir).unwrap();

    // HEAD and the branch ref are both present and resolve to the same tip.
    let head = refs
        .iter()
        .find(|(n, _)| n == "HEAD")
        .map(|(_, o)| *o)
        .expect("HEAD present");
    let branch = refs
        .iter()
        .find(|(n, _)| n.starts_with("refs/heads/"))
        .map(|(_, o)| *o)
        .expect("a branch ref present");
    assert_eq!(head, branch, "HEAD should resolve to the branch tip");

    // Refs are returned in sorted order (deterministic).
    let mut sorted = refs.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(refs, sorted, "read_refs must be name-sorted");
}

#[test]
fn commit_graph_import_records_parent_edge() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping commit-graph test");
        return;
    }
    let (_work, git_dir) = two_commit_repo();
    let omo_dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(omo_dir.path()).unwrap();
    let import = import_repo(&git_dir, &repo).unwrap();

    // Two commits walked, with one parent edge between them.
    assert_eq!(import.commits, 2, "expected exactly 2 commits in the DAG");
    let child = import
        .commit_dag
        .iter()
        .find(|(_, c)| c.parents.len() == 1)
        .map(|(oid, c)| (*oid, c.parents[0]))
        .expect("a child commit with one parent");
    // The child's parent is the other commit in the DAG (a real child→parent edge).
    assert!(
        import.commit_dag.contains_key(&child.1),
        "child's parent must be in the DAG"
    );
    // The root commit has no parents.
    assert!(
        import.commit_dag.values().any(|c| c.parents.is_empty()),
        "expected a root commit with no parents"
    );

    // Every reachable blob and tree was imported (2 files + a subtree + root).
    assert!(
        import.blobs >= 2,
        "expected >=2 blobs, got {}",
        import.blobs
    );
    assert!(
        import.trees >= 2,
        "expected >=2 trees (root + sub), got {}",
        import.trees
    );

    // commit_log is newest-first: the child (2 parents-of chain) precedes its parent.
    let log = import.commit_log();
    assert_eq!(log.len(), 2);
    let child_pos = log.iter().position(|o| *o == child.0).unwrap();
    let parent_pos = log.iter().position(|o| *o == child.1).unwrap();
    assert!(
        child_pos < parent_pos,
        "child must come before parent (newest-first)"
    );
}

#[test]
fn repo_level_roundtrip_export_is_byte_identical() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping repo-level round-trip test");
        return;
    }
    let (_work, git_dir) = two_commit_repo();

    // The gate passes over the real repo.
    let report = verify_repo(&git_dir).unwrap();
    assert!(report.commits >= 2);
    assert_eq!(report.packfiles, 0, "fresh repo should have no packfiles");

    // Import walks the whole graph.
    let omo_dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(omo_dir.path()).unwrap();
    let import = import_repo(&git_dir, &repo).unwrap();

    // Export to a fresh dir and assert the object set is byte-identical.
    let out = tempfile::tempdir().unwrap();
    let export = export_repo(&import, out.path()).unwrap();
    assert!(
        export.objects >= 5,
        "expected all reachable objects exported"
    );
    assert!(
        export_matches_source(&git_dir, out.path()).unwrap(),
        "exported loose objects must be byte-identical to the source"
    );
}
