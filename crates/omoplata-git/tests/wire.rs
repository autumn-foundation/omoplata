//! Integration tests for the git wire protocol (`upload-pack` fetch) over the
//! local transport, guarded on `git` being on PATH. If git is absent, each test
//! prints a note and passes — a missing tool never fails the suite.

use std::path::Path;
use std::process::Command;

use omoplata_git::{fetch_local, oid, GitObject};
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

/// Capture `git <args>` stdout as a trimmed string.
fn git_capture(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).expect("git output utf-8")
}

/// Build a 2-commit, 2-file repo (a parent edge and a subtree) and return the
/// worktree root plus the tempdir keeping it alive.
fn two_commit_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().to_path_buf();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a.txt"), b"first\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub").join("b.txt"), b"nested\n").unwrap();
    git(&root, &["add", "-A"]);
    git_commit(&root, "first commit");
    std::fs::write(root.join("a.txt"), b"first\nsecond\n").unwrap();
    git(&root, &["add", "-A"]);
    git_commit(&root, "second commit");
    (work, root)
}

/// The number of reachable objects git reports across all refs.
fn git_object_count(dir: &Path) -> usize {
    git_capture(dir, &["rev-list", "--objects", "--all"])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

#[test]
fn fetch_local_clones_over_the_wire() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping wire fetch test");
        return;
    }
    let (_work, root) = two_commit_repo();

    // The oid HEAD/main should advertise.
    let head_oid = git_capture(&root, &["rev-parse", "HEAD"]).trim().to_owned();

    // Fetch into a fresh omoplata repo, addressing the source by local path.
    let omo_dir = tempfile::tempdir().unwrap();
    let dest = Repository::init(omo_dir.path()).unwrap();
    let fetch = fetch_local(root.to_str().unwrap(), &dest).expect("wire fetch succeeds");

    // The advertisement includes HEAD and refs/heads/main, both at the tip oid.
    let head = fetch
        .refs
        .iter()
        .find(|(n, _)| n == "HEAD")
        .map(|(_, o)| o.hex())
        .expect("HEAD advertised");
    assert_eq!(head, head_oid, "HEAD advertised at the right oid");
    let main = fetch
        .refs
        .iter()
        .find(|(n, _)| n == "refs/heads/main")
        .map(|(_, o)| o.hex())
        .expect("refs/heads/main advertised");
    assert_eq!(main, head_oid, "main advertised at the right oid");

    // A non-empty packfile arrived over the wire.
    assert!(fetch.pack_bytes > 0, "packfile bytes should be > 0");

    // Two commits imported, with the file/subtree objects.
    assert_eq!(fetch.import.commits, 2, "both commits imported");
    assert!(fetch.import.blobs >= 2, "expected >=2 blobs");
    assert!(fetch.import.trees >= 2, "expected root + sub trees");

    // Every reachable object git knows about was received/imported: the sum of
    // blobs+trees+commits (+tags) equals `git rev-list --objects --all`.
    let expected = git_object_count(&root);
    let imported =
        fetch.import.blobs + fetch.import.trees + fetch.import.commits + fetch.import.tags;
    assert_eq!(
        imported, expected,
        "imported object count must match git rev-list --objects --all"
    );

    // Every imported blob's recomputed oid matches, and its bytes are in the
    // store (a spot check that the reconstructed objects are real).
    let (blob_oid, want) = fetch
        .import
        .git_objects
        .iter()
        .find_map(|(o, obj)| match obj {
            GitObject::Blob(b) => Some((*o, b.clone())),
            _ => None,
        })
        .expect("a blob was imported");
    // Recomputed oid matches the advertised/received oid.
    assert_eq!(oid(&GitObject::Blob(want.clone())), blob_oid);
    let store_id = fetch.import.oid_map.get(&blob_oid).expect("blob mapped");
    match dest.read_object(store_id).unwrap() {
        Object::Blob(b) => assert_eq!(b.bytes(), want.as_slice()),
        other => panic!("expected blob, got {other:?}"),
    }
}

#[test]
fn fetch_local_accepts_file_url() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping file:// url test");
        return;
    }
    let (_work, root) = two_commit_repo();
    let url = format!("file://{}", root.display());

    let omo_dir = tempfile::tempdir().unwrap();
    let dest = Repository::init(omo_dir.path()).unwrap();
    let fetch = fetch_local(&url, &dest).expect("wire fetch via file:// url");
    assert_eq!(fetch.import.commits, 2);
    assert!(fetch.pack_bytes > 0);
}

#[test]
fn fetch_local_from_gc_packed_repo() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping gc'd wire fetch test");
        return;
    }
    // A repo whose objects are packed (and delta-compressed) on the server side.
    // upload-pack still streams a fresh, self-contained pack; the receiving
    // decoder must handle offset deltas.
    let work = tempfile::tempdir().unwrap();
    let root = work.path().to_path_buf();
    git(&root, &["init", "-q", "-b", "main"]);

    let mut lines: Vec<String> = (0..200)
        .map(|i| format!("line {i}: original content"))
        .collect();
    for rev in 0..6 {
        for i in (rev * 10)..(rev * 10 + 10) {
            if let Some(slot) = lines.get_mut(i) {
                *slot = format!("line {i}: revised at rev {rev}");
            }
        }
        std::fs::write(root.join("big.txt"), lines.join("\n") + "\n").unwrap();
        std::fs::write(
            root.join(format!("file_{rev}.txt")),
            format!("contents of file {rev}\n").repeat(20),
        )
        .unwrap();
        git(&root, &["add", "-A"]);
        git_commit(&root, &format!("commit {rev}"));
    }
    // Force a delta-compressed packfile on the server side.
    git(&root, &["repack", "-adq"]);

    let head_oid = git_capture(&root, &["rev-parse", "HEAD"]).trim().to_owned();

    let omo_dir = tempfile::tempdir().unwrap();
    let dest = Repository::init(omo_dir.path()).unwrap();
    let fetch = fetch_local(root.to_str().unwrap(), &dest).expect("wire fetch from packed repo");

    assert!(fetch.pack_bytes > 0);
    assert_eq!(
        fetch.import.commits, 6,
        "all six commits imported over wire"
    );

    // HEAD advertised at the right oid.
    let head = fetch
        .refs
        .iter()
        .find(|(n, _)| n == "HEAD")
        .map(|(_, o)| o.hex())
        .expect("HEAD advertised");
    assert_eq!(head, head_oid);

    // Object count matches git's own reachability count.
    let expected = git_object_count(&root);
    let imported =
        fetch.import.blobs + fetch.import.trees + fetch.import.commits + fetch.import.tags;
    assert_eq!(imported, expected, "wire clone imports the full object set");
}
