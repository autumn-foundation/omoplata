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
fn defs_lists_definitions_in_source_order() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(
        &file,
        "struct Point { x: i32 }\nfn free() {}\nmod inner {\n    fn nested() {}\n}\n",
    )
    .unwrap();
    omo()
        .arg("defs")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("struct Point (lines 1-1)"))
        .stdout(predicate::str::contains("fn free (lines 2-2)"))
        .stdout(predicate::str::contains("mod inner"))
        .stdout(predicate::str::contains("fn inner::nested"));
}

#[test]
fn track_detects_rename() {
    let dir = tempdir().unwrap();
    let old = dir.path().join("old.rs");
    let new = dir.path().join("new.rs");
    std::fs::write(&old, "fn foo() { let x = 41 + 1; }\n").unwrap();
    std::fs::write(&new, "fn bar() { let x = 41 + 1; }\n").unwrap();
    omo()
        .arg("track")
        .arg(&old)
        .arg(&new)
        .assert()
        .success()
        .stdout(predicate::str::contains("renamed foo -> bar (fn)"));
}

#[test]
fn ref_set_list_undo_and_op_log() {
    let dir = tempdir().unwrap();
    omo().arg("init").arg(dir.path()).assert().success();

    let a = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    let b = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

    // Set two refs.
    omo()
        .args(["ref", "set", "a", a, "--repo"])
        .arg(dir.path())
        .assert()
        .success();
    omo()
        .args(["ref", "set", "b", b, "--repo"])
        .arg(dir.path())
        .assert()
        .success();

    // `ref list` shows both.
    omo()
        .args(["ref", "list", "--repo"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("a {a}")))
        .stdout(predicate::str::contains(format!("b {b}")));

    // `revset 'a | b'` prints the two commit ids.
    omo()
        .args(["revset", "a | b", "--repo"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(a))
        .stdout(predicate::str::contains(b));

    // `op undo` reverts the last op (deletes ref b).
    omo()
        .args(["op", "undo", "--repo"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("undo of #1"));

    // `ref list` now shows only a.
    omo()
        .args(["ref", "list", "--repo"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("a {a}")))
        .stdout(predicate::str::contains("b ").not());

    // `op log` shows the growing history (3 entries: two sets + the undo).
    omo()
        .args(["op", "log", "--repo"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("#2 undo #1"))
        .stdout(predicate::str::contains("#1 set-ref b"))
        .stdout(predicate::str::contains("#0 set-ref a"));
}

#[test]
fn revset_unknown_ref_fails() {
    let dir = tempdir().unwrap();
    omo().arg("init").arg(dir.path()).assert().success();
    omo()
        .args(["revset", "nope", "--repo"])
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown ref"));
}

#[test]
fn merge_file_rust_structural_downgraded_by_kernel() {
    // Both sides append a new top-level fn at the same location: the Rust
    // structural driver (an untrusted proposer) merges cleanly, but the two
    // appends land at the same line-level anchor, so the trusted kernel cannot
    // produce an independent commutation witness for the driver's chosen order.
    // The LCF gate downgrades the proposal to a conflict (I8): the driver's
    // clean output still appears on stdout, but the kernel refuses to admit it
    // and the exit code is non-zero.
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.rs");
    let left = dir.path().join("left.rs");
    let right = dir.path().join("right.rs");
    std::fs::write(&base, "fn a() {}\n\nfn b() {}\n").unwrap();
    std::fs::write(&left, "fn a() {}\n\nfn b() {}\n\nfn c() {}\n").unwrap();
    std::fs::write(&right, "fn a() {}\n\nfn b() {}\n\nfn d() {}\n").unwrap();
    omo()
        .arg("merge-file")
        .arg(&base)
        .arg(&left)
        .arg(&right)
        .assert()
        .failure()
        .stdout(predicate::str::contains("fn c()"))
        .stdout(predicate::str::contains("fn d()"))
        .stderr(predicate::str::contains(
            "rust-structural merge: 0 conflict(s)",
        ))
        .stderr(predicate::str::contains("kernel: downgraded to conflict"));
}

#[test]
fn merge_file_rust_disjoint_kernel_admitted() {
    // Disjoint edits (each side edits a different definition's body) are
    // disjoint-support at the line level: the structural driver merges cleanly
    // and the trusted kernel independently witnesses the merge, admitting it.
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.rs");
    let left = dir.path().join("left.rs");
    let right = dir.path().join("right.rs");
    std::fs::write(&base, "fn a() {}\n\nfn b() {}\n").unwrap();
    std::fs::write(&left, "fn a() { let x = 1; }\n\nfn b() {}\n").unwrap();
    std::fs::write(&right, "fn a() {}\n\nfn b() { let y = 2; }\n").unwrap();
    omo()
        .arg("merge-file")
        .arg(&base)
        .arg(&left)
        .arg(&right)
        .assert()
        .success()
        .stdout(predicate::str::contains("let x = 1;"))
        .stdout(predicate::str::contains("let y = 2;"))
        .stderr(predicate::str::contains(
            "rust-structural merge: 0 conflict(s)",
        ))
        .stderr(predicate::str::contains("kernel: admitted"));
}

#[test]
fn admit_disjoint_edits_exits_zero_with_witness() {
    // `omo admit` runs the trusted kernel directly on three files. Disjoint
    // edits commute, so the kernel admits with a commutation witness (exit 0),
    // printing the merged document with both edits.
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.txt");
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&base, "a\nb\nc\nd\n").unwrap();
    std::fs::write(&left, "A\nb\nc\nd\n").unwrap(); // edits line 1
    std::fs::write(&right, "a\nb\nc\nD\n").unwrap(); // edits line 4
    omo()
        .arg("admit")
        .arg(&base)
        .arg(&left)
        .arg(&right)
        .assert()
        .success()
        .stdout("A\nb\nc\nD\n")
        .stderr(predicate::str::contains(
            "admitted: commutation witness (support: 1 hunks p, 1 hunks q)",
        ));
}

#[test]
fn admit_conflicting_edits_exits_nonzero() {
    // Overlapping edits do not commute: the kernel refuses to admit and reports
    // a conflict (exit non-zero) — never a silent merge.
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.txt");
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&base, "a\nb\nc\n").unwrap();
    std::fs::write(&left, "a\nX\nc\n").unwrap();
    std::fs::write(&right, "a\nY\nc\n").unwrap();
    omo()
        .arg("admit")
        .arg(&base)
        .arg(&left)
        .arg(&right)
        .assert()
        .failure()
        .stdout(predicate::str::contains("<<<<<<< left"))
        .stderr(predicate::str::contains("conflict: 1 region(s)"));
}

#[test]
fn merge_file_txt_line_conflict_exits_nonzero() {
    // A non-.rs path uses the line fallback; a same-line divergent edit
    // conflicts and exits non-zero.
    let dir = tempdir().unwrap();
    let base = dir.path().join("base.txt");
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    std::fs::write(&base, "a\nb\nc\n").unwrap();
    std::fs::write(&left, "a\nX\nc\n").unwrap();
    std::fs::write(&right, "a\nY\nc\n").unwrap();
    omo()
        .arg("merge-file")
        .arg(&base)
        .arg(&left)
        .arg(&right)
        .assert()
        .failure()
        .stdout(predicate::str::contains("<<<<<<< left"))
        .stderr(predicate::str::contains("line merge: 1 conflict(s)"));
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

/// Whether `git` is available on PATH (the git-interop CLI tests need it).
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

#[test]
fn git_verify_and_import_against_real_repo() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping git verify/import CLI test");
        return;
    }
    // Build a real git repo.
    let work = tempdir().unwrap();
    let root = work.path();
    run_git(root, &["init", "-q"]);
    std::fs::write(root.join("f.txt"), b"content\n").unwrap();
    run_git(root, &["add", "f.txt"]);
    run_git(
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

    // `omo git verify` exits 0 and reports PASS.
    omo()
        .arg("git")
        .arg("verify")
        .arg(&git_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("round-trip gate: PASS"))
        .stdout(predicate::str::contains("blobs:"));

    // `omo git import` into a fresh omoplata repo reports >=1 blob.
    let omo_dir = tempdir().unwrap();
    omo().arg("init").arg(omo_dir.path()).assert().success();
    omo()
        .arg("git")
        .arg("import")
        .arg(&git_dir)
        .arg("--repo")
        .arg(omo_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"imported blobs:\s+[1-9]").unwrap())
        .stdout(predicate::str::contains("git -> omoplata mappings:"));
}

fn git_commit(dir: &std::path::Path, message: &str) {
    run_git(
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

#[test]
fn git_log_and_export_against_two_commit_repo() {
    if !git_available() {
        eprintln!("note: `git` not on PATH; skipping git log/export CLI test");
        return;
    }
    // A 2-commit, 2-file repo (a parent edge and a subtree).
    let work = tempdir().unwrap();
    let root = work.path();
    run_git(root, &["init", "-q"]);
    std::fs::write(root.join("a.txt"), b"first\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub").join("b.txt"), b"nested\n").unwrap();
    run_git(root, &["add", "-A"]);
    git_commit(root, "first commit");
    std::fs::write(root.join("a.txt"), b"first\nsecond\n").unwrap();
    run_git(root, &["add", "-A"]);
    git_commit(root, "second commit");
    let git_dir = root.join(".git");

    // `omo git log` lists 2 commits, newest-first, with the parent edge shown.
    let out = omo().arg("git").arg("log").arg(&git_dir).output().unwrap();
    assert!(out.status.success());
    let log = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 commits in log, got: {log:?}");
    assert!(lines[0].contains("second commit"), "newest first: {log:?}");
    assert!(lines[1].contains("first commit"), "root last: {log:?}");
    // The newest commit lists a parent; the root's parents are "-".
    assert!(
        lines[0].contains("(parents: "),
        "child must show a parent: {log:?}"
    );
    assert!(
        lines[1].contains("(parents: -)"),
        "root must have no parents: {log:?}"
    );

    // `omo git export` prints PASS and exits 0.
    let out_dir = tempdir().unwrap();
    omo()
        .arg("git")
        .arg("export")
        .arg(&git_dir)
        .arg(out_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("round-trip vs source: PASS"))
        .stdout(predicate::str::is_match(r"exported \d+ objects").unwrap());
}

/// Two files whose first function is essentially the same under a different
/// name, plus an unrelated function in each — the fixtures for `dup`/`similar`.
const FILE_A: &str = "fn area_of_rect(width: f64, height: f64) -> f64 {\n    let area = width * height;\n    area\n}\n\nfn greet(name: &str) -> String {\n    format!(\"hello, {name}!\")\n}\n";
const FILE_B: &str = "fn rectangle_area(width: f64, height: f64) -> f64 {\n    let area = width * height;\n    area\n}\n\nfn factorial(n: u64) -> u64 {\n    let mut acc = 1;\n    for k in 2..=n {\n        acc *= k;\n    }\n    acc\n}\n";

#[test]
fn dup_flags_the_near_identical_pair() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    std::fs::write(&a, FILE_A).unwrap();
    std::fs::write(&b, FILE_B).unwrap();

    // The two rectangle-area functions are flagged as a cross-file duplicate;
    // the unrelated greet/factorial are not.
    omo()
        .arg("dup")
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout(predicate::str::contains("area_of_rect"))
        .stdout(predicate::str::contains("rectangle_area"))
        .stdout(predicate::str::contains(" ~ "))
        .stdout(predicate::str::contains("factorial").not());
}

#[test]
fn dup_reports_none_when_all_distinct() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("only.rs");
    std::fs::write(
        &a,
        "fn greet(name: &str) -> String {\n    format!(\"hi {name}\")\n}\n",
    )
    .unwrap();
    omo()
        .arg("dup")
        .arg(&a)
        .assert()
        .success()
        .stdout(predicate::str::contains("no likely duplicate definitions"));
}

#[test]
fn similar_ranks_the_rectangle_function_first() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    std::fs::write(&a, FILE_A).unwrap();
    std::fs::write(&b, FILE_B).unwrap();

    // The top hit for a rectangle-area query is one of the area functions.
    let out = omo()
        .arg("similar")
        .arg("compute area of rectangle")
        .arg(&a)
        .arg(&b)
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let first = stdout.lines().next().unwrap_or("");
    assert!(
        first.contains("area_of_rect") || first.contains("rectangle_area"),
        "expected an area function ranked first, got: {first:?}"
    );
}
