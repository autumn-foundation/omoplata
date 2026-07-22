//! End-to-end pipeline test driving the `omo` binary across the whole crate
//! stack: store, algebra, drivers, identity, work, git, and sem. One realistic
//! run asserts real output at each step. The git leg is guarded on `git` being
//! on PATH and is skipped gracefully when it is absent.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn omo() -> Command {
    Command::cargo_bin("omo").unwrap()
}

/// Whether `git` is available on PATH (the git-interop leg needs it).
fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// A realistic definition whose second version renames it — exercises `track`.
const OLD_RS: &str =
    "fn compute_area(w: f64, h: f64) -> f64 {\n    let area = w * h;\n    area\n}\n";
const NEW_RS: &str =
    "fn rectangle_area(w: f64, h: f64) -> f64 {\n    let area = w * h;\n    area\n}\n";

/// Near-duplicate files for `dup`/`similar`: the first function of each is the
/// same computation under a different name; the second is unrelated.
const DUP_A: &str = "fn area_of_rect(width: f64, height: f64) -> f64 {\n    let area = width * height;\n    area\n}\n\nfn greet(name: &str) -> String {\n    format!(\"hello, {name}!\")\n}\n";
const DUP_B: &str = "fn rectangle_area(width: f64, height: f64) -> f64 {\n    let area = width * height;\n    area\n}\n\nfn factorial(n: u64) -> u64 {\n    let mut acc = 1;\n    for k in 2..=n {\n        acc *= k;\n    }\n    acc\n}\n";

#[test]
fn end_to_end_pipeline() {
    let work = tempdir().unwrap();
    let repo = work.path();

    // 1. init + status --------------------------------------------------------
    omo()
        .arg("init")
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Initialized empty omoplata repository",
        ));
    assert!(repo.join(".omoplata").is_dir());
    omo()
        .arg("status")
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("On omoplata repository"));

    // 2. hash-object round-trips through cat-object ---------------------------
    let payload = b"the unit of version control is the definition\n";
    let file = repo.join("thesis.txt");
    std::fs::write(&file, payload).unwrap();
    let out = omo()
        .args(["hash-object", "--repo"])
        .arg(repo)
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8(out.stdout).unwrap();
    let id = id.trim().to_string();
    assert!(id.starts_with("sha256:"), "unexpected id: {id}");
    omo()
        .args(["cat-object", "--repo"])
        .arg(repo)
        .arg(&id)
        .assert()
        .success()
        .stdout(predicate::eq(payload.as_slice()));

    // 3. git verify + import against a real git repo (guarded) ----------------
    if git_available() {
        let gitwork = tempdir().unwrap();
        let groot = gitwork.path();
        run_git(groot, &["init", "-q"]);
        std::fs::write(groot.join("f.txt"), b"imported content\n").unwrap();
        run_git(groot, &["add", "f.txt"]);
        run_git(
            groot,
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
        let git_dir = groot.join(".git");

        omo()
            .arg("git")
            .arg("verify")
            .arg(&git_dir)
            .assert()
            .success()
            .stdout(predicate::str::contains("round-trip gate: PASS"))
            .stdout(predicate::str::contains("blobs:"));

        omo()
            .arg("git")
            .arg("import")
            .arg(&git_dir)
            .arg("--repo")
            .arg(repo)
            .assert()
            .success()
            .stdout(predicate::str::is_match(r"imported blobs:\s+[1-9]").unwrap())
            .stdout(predicate::str::contains("git -> omoplata mappings:"));
    } else {
        eprintln!("note: `git` not on PATH; skipping git verify/import leg of e2e");
    }

    // 4. defs lists definitions; track detects the rename ---------------------
    let old = repo.join("old.rs");
    let new = repo.join("new.rs");
    std::fs::write(&old, OLD_RS).unwrap();
    std::fs::write(&new, NEW_RS).unwrap();
    omo()
        .arg("defs")
        .arg(&old)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn compute_area (lines 1-4)"));
    omo()
        .arg("track")
        .arg(&old)
        .arg(&new)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "renamed compute_area -> rectangle_area (fn)",
        ));

    // 5. merge-file structurally merges two divergent Rust edits cleanly ------
    let mbase = repo.join("m_base.rs");
    let mleft = repo.join("m_left.rs");
    let mright = repo.join("m_right.rs");
    std::fs::write(&mbase, "fn a() {}\n\nfn b() {}\n").unwrap();
    std::fs::write(&mleft, "fn a() {}\n\nfn b() {}\n\nfn c() {}\n").unwrap();
    std::fs::write(&mright, "fn a() {}\n\nfn b() {}\n\nfn d() {}\n").unwrap();
    omo()
        .arg("merge-file")
        .arg(&mbase)
        .arg(&mleft)
        .arg(&mright)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn c()"))
        .stdout(predicate::str::contains("fn d()"))
        .stderr(predicate::str::contains(
            "rust-structural merge: 0 conflict(s)",
        ));

    // 6. ref set -> op log -> op undo -> revset -------------------------------
    let commit = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    omo()
        .args(["ref", "set", "main", commit, "--repo"])
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("set-ref main"));
    omo()
        .args(["ref", "list", "--repo"])
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("main {commit}")));
    omo()
        .args(["revset", "main", "--repo"])
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains(commit));
    omo()
        .args(["op", "log", "--repo"])
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("#0 set-ref main"));
    omo()
        .args(["op", "undo", "--repo"])
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("undo of #0"));
    // After undo, the ref is gone.
    omo()
        .args(["ref", "list", "--repo"])
        .arg(repo)
        .assert()
        .success()
        .stdout(predicate::str::contains("main ").not());

    // 7. dup + similar flag the near-duplicate definition pair ----------------
    let da = repo.join("dup_a.rs");
    let db = repo.join("dup_b.rs");
    std::fs::write(&da, DUP_A).unwrap();
    std::fs::write(&db, DUP_B).unwrap();
    omo()
        .arg("dup")
        .arg(&da)
        .arg(&db)
        .assert()
        .success()
        .stdout(predicate::str::contains("area_of_rect"))
        .stdout(predicate::str::contains("rectangle_area"))
        .stdout(predicate::str::contains(" ~ "))
        .stdout(predicate::str::contains("factorial").not());

    let out = omo()
        .arg("similar")
        .arg("compute area of rectangle")
        .arg(&da)
        .arg(&db)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let first = stdout.lines().next().unwrap_or("");
    assert!(
        first.contains("area_of_rect") || first.contains("rectangle_area"),
        "expected an area function ranked first, got: {first:?}"
    );
}
