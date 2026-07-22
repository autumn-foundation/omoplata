//! Unit tests for the Tier-2 drivers, focused on the structural-vs-line win.

use crate::{
    mergiraf_available, parse_diff3_conflicts, select_driver, supports_extension, LineDriver,
    MergeDriver, MergeInput, MergirafDriver, RustStructuralDriver,
};

fn structural(base: &str, left: &str, right: &str) -> crate::DriverOutput {
    RustStructuralDriver::new()
        .merge(&MergeInput {
            base,
            left,
            right,
            path: "lib.rs",
        })
        .expect("structural merge")
}

fn line(base: &str, left: &str, right: &str) -> crate::DriverOutput {
    LineDriver::new()
        .merge(&MergeInput {
            base,
            left,
            right,
            path: "notes.txt",
        })
        .expect("line merge")
}

/// The killer case (design doc §4 Tier 2): both sides append a new top-level
/// item at the same textual location. The line driver conflicts; the structural
/// driver merges cleanly with all four functions present.
#[test]
fn structural_beats_line_on_two_sided_append() {
    let base = "fn a() {}\n\nfn b() {}\n";
    let left = "fn a() {}\n\nfn b() {}\n\nfn c() {}\n";
    let right = "fn a() {}\n\nfn b() {}\n\nfn d() {}\n";

    // Line driver: appending different tails at the same spot conflicts.
    let l = line(base, left, right);
    assert!(
        !l.is_clean(),
        "line driver should conflict, got: {}",
        l.merged
    );
    assert_eq!(l.driver, "line");

    // Structural driver: clean, all four definitions present, in order.
    let s = structural(base, left, right);
    assert!(
        s.is_clean(),
        "structural should be clean, got: {}",
        s.merged
    );
    assert_eq!(s.driver, "rust-structural");
    for f in ["fn a()", "fn b()", "fn c()", "fn d()"] {
        assert!(s.merged.contains(f), "missing {f} in:\n{}", s.merged);
    }
    // c before d (left-added before right-added, canonical order).
    let ci = s.merged.find("fn c()").unwrap();
    let di = s.merged.find("fn d()").unwrap();
    assert!(ci < di, "expected c before d in:\n{}", s.merged);
}

/// Disjoint edits: left edits the body of `fn a`, right edits the body of
/// `fn b`. Structural merge is clean and carries both edits.
#[test]
fn structural_clean_on_disjoint_body_edits() {
    let base = "fn a() { 1 }\n\nfn b() { 1 }\n";
    let left = "fn a() { 2 }\n\nfn b() { 1 }\n";
    let right = "fn a() { 1 }\n\nfn b() { 2 }\n";

    let s = structural(base, left, right);
    assert!(s.is_clean(), "expected clean, got:\n{}", s.merged);
    assert!(
        s.merged.contains("fn a() { 2 }"),
        "left edit missing:\n{}",
        s.merged
    );
    assert!(
        s.merged.contains("fn b() { 2 }"),
        "right edit missing:\n{}",
        s.merged
    );
}

/// True conflict: both sides edit the body of `fn a` differently. The structural
/// driver emits a conflict for that definition rather than guessing.
#[test]
fn structural_conflicts_on_two_sided_body_edit() {
    let base = "fn a() { 1 }\n\nfn b() { 0 }\n";
    let left = "fn a() { 2 }\n\nfn b() { 0 }\n";
    let right = "fn a() { 3 }\n\nfn b() { 0 }\n";

    let s = structural(base, left, right);
    assert!(
        !s.is_clean(),
        "expected a conflict, got clean:\n{}",
        s.merged
    );
    assert!(
        s.merged.contains("<<<<<<< left"),
        "no markers in:\n{}",
        s.merged
    );
    // fn b, untouched, stays clean and present.
    assert!(s.merged.contains("fn b() { 0 }"));
}

/// Delete/modify: left deletes `fn a`, right edits `fn a` ⇒ conflict.
#[test]
fn structural_conflicts_on_delete_modify() {
    let base = "fn a() { 1 }\n\nfn b() { 0 }\n";
    let left = "fn b() { 0 }\n"; // fn a deleted
    let right = "fn a() { 2 }\n\nfn b() { 0 }\n"; // fn a edited

    let s = structural(base, left, right);
    assert!(
        !s.is_clean(),
        "expected a delete/modify conflict, got:\n{}",
        s.merged
    );
    let c = &s.conflicts[0];
    // The deletion side is empty; the modify side carries the edited body.
    assert!(c.left.is_empty(), "left (deleted) should be empty: {c:?}");
    assert!(
        c.right.iter().any(|l| l.contains("fn a() { 2 }")),
        "right should carry the modification: {c:?}"
    );
}

/// Both sides add the same-named item identically ⇒ included once, clean.
#[test]
fn structural_clean_on_identical_two_sided_add() {
    let base = "fn a() {}\n";
    let left = "fn a() {}\n\nfn c() { 1 }\n";
    let right = "fn a() {}\n\nfn c() { 1 }\n";

    let s = structural(base, left, right);
    assert!(
        s.is_clean(),
        "identical adds should be clean:\n{}",
        s.merged
    );
    assert_eq!(
        s.merged.matches("fn c()").count(),
        1,
        "c should appear once:\n{}",
        s.merged
    );
}

/// Both sides add a same-named item with differing bodies ⇒ conflict.
#[test]
fn structural_conflicts_on_differing_two_sided_add() {
    let base = "fn a() {}\n";
    let left = "fn a() {}\n\nfn c() { 1 }\n";
    let right = "fn a() {}\n\nfn c() { 2 }\n";

    let s = structural(base, left, right);
    assert!(
        !s.is_clean(),
        "differing adds should conflict:\n{}",
        s.merged
    );
}

/// The structural output reassembles faithfully when nothing changes.
#[test]
fn structural_identity_merge_roundtrips() {
    let base = "fn a() { 1 }\n\nfn b() { 2 }\n";
    let s = structural(base, base, base);
    assert!(s.is_clean());
    assert_eq!(s.merged, base, "identity merge should reproduce the input");
}

/// Unparseable Rust falls back to the line driver (documented behavior).
#[test]
fn structural_falls_back_to_line_on_invalid_rust() {
    let base = "fn a( {\n"; // not valid Rust
    let out = structural(base, base, base);
    assert_eq!(
        out.driver, "line",
        "should have fallen back to the line driver"
    );
}

/// `select_driver` routes `.rs` to the structural driver regardless of Mergiraf.
#[test]
fn select_driver_routes_rust_to_structural() {
    assert_eq!(select_driver("x.rs").name(), "rust-structural");
    assert_eq!(select_driver("src/lib.rs").name(), "rust-structural");
}

/// `select_driver` routes an *unsupported* extension to the line driver whether
/// or not Mergiraf is present (`.txt` and no-extension are not Mergiraf grammars).
#[test]
fn select_driver_routes_unsupported_to_line() {
    assert_eq!(select_driver("x.txt").name(), "line");
    assert_eq!(select_driver("README").name(), "line");
    // A `.rs` suffix that is not the final extension is not Rust, and `.bak` is
    // not a Mergiraf grammar ⇒ line driver.
    assert_eq!(select_driver("archive.rs.bak").name(), "line");
}

/// `select_driver` routes a Mergiraf-supported non-Rust extension to the
/// Mergiraf driver when the binary is present, and to the line driver when it is
/// absent. We cannot easily fake absence in-process, so this asserts whichever
/// branch the environment is in; the guarded integration tests below exercise
/// the real Mergiraf merge when the binary is installed.
#[test]
fn select_driver_routes_supported_extension_by_availability() {
    let name = select_driver("config.json").name();
    if mergiraf_available() {
        assert_eq!(
            name, "mergiraf",
            "json should route to mergiraf when present"
        );
    } else {
        assert_eq!(name, "line", "json should fall back to line when absent");
    }
}

/// `supports_extension` recognises Mergiraf grammars (case-insensitively) and
/// rejects unsupported / extension-less paths.
#[test]
fn supports_extension_recognises_grammars() {
    for p in ["a.json", "b.java", "c.go", "d.yaml", "e.TS", "dir/f.toml"] {
        assert!(supports_extension(p), "{p} should be supported");
    }
    for p in ["a.txt", "README", "notes", "x.unknownext"] {
        assert!(!supports_extension(p), "{p} should be unsupported");
    }
}

/// A single diff3 block parses into one `Conflict` with the right sections.
#[test]
fn parse_diff3_single_block() {
    let merged = "\
common before
<<<<<<< left
left one
left two
||||||| base
base one
=======
right one
>>>>>>> right
common after
";
    let conflicts = parse_diff3_conflicts(merged);
    assert_eq!(conflicts.len(), 1);
    let c = &conflicts[0];
    assert_eq!(c.left, vec!["left one", "left two"]);
    assert_eq!(c.base, vec!["base one"]);
    assert_eq!(c.right, vec!["right one"]);
}

/// Multiple blocks parse in order; clean text between and around them is ignored.
#[test]
fn parse_diff3_multiple_blocks() {
    let merged = "\
<<<<<<< left
L1
||||||| base
B1
=======
R1
>>>>>>> right
middle line
<<<<<<< left
L2
||||||| base
=======
R2
>>>>>>> right
";
    let conflicts = parse_diff3_conflicts(merged);
    assert_eq!(conflicts.len(), 2);
    assert_eq!(conflicts[0].left, vec!["L1"]);
    assert_eq!(conflicts[0].base, vec!["B1"]);
    assert_eq!(conflicts[0].right, vec!["R1"]);
    // Second block has an empty base section.
    assert_eq!(conflicts[1].left, vec!["L2"]);
    assert!(conflicts[1].base.is_empty());
    assert_eq!(conflicts[1].right, vec!["R2"]);
}

/// A block with no `|||||||` base marker (2-way form) still parses: base empty,
/// left runs until `=======`, right until `>>>>>>>`.
#[test]
fn parse_diff3_two_way_block_has_empty_base() {
    let merged = "\
<<<<<<< left
L
=======
R
>>>>>>> right
";
    let conflicts = parse_diff3_conflicts(merged);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].left, vec!["L"]);
    assert!(conflicts[0].base.is_empty());
    assert_eq!(conflicts[0].right, vec!["R"]);
}

/// An unterminated conflict block is dropped rather than panicking; clean input
/// yields no conflicts.
#[test]
fn parse_diff3_degrades_gracefully() {
    assert!(parse_diff3_conflicts("just clean text\nno markers\n").is_empty());
    let unterminated = "<<<<<<< left\nL\n||||||| base\nB\n=======\nR\n";
    assert!(
        parse_diff3_conflicts(unterminated).is_empty(),
        "unterminated block should be dropped"
    );
}

/// Guarded integration test (skips when `mergiraf` is not on PATH): two disjoint
/// structural edits to a JSON object that a plain line merge conflicts on are
/// merged **cleanly** by Mergiraf, and the result carries both edits.
#[test]
fn mergiraf_merges_disjoint_json_cleanly() {
    if !mergiraf_available() {
        eprintln!("skipping: mergiraf not on PATH");
        return;
    }
    // base has two keys; left edits the first, right edits the second. A line
    // merge conflicts because both touch adjacent lines; Mergiraf resolves it
    // structurally.
    let base = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
    let left = "{\n  \"a\": 10,\n  \"b\": 2\n}\n";
    let right = "{\n  \"a\": 1,\n  \"b\": 20\n}\n";
    let out = MergirafDriver::new()
        .merge(&MergeInput {
            base,
            left,
            right,
            path: "config.json",
        })
        .expect("mergiraf merge");
    assert!(
        out.is_clean(),
        "expected a clean structural merge, got conflicts:\n{}",
        out.merged
    );
    assert_eq!(out.driver, "mergiraf");
    assert!(
        out.merged.contains("10"),
        "left edit missing:\n{}",
        out.merged
    );
    assert!(
        out.merged.contains("20"),
        "right edit missing:\n{}",
        out.merged
    );
}

/// Guarded integration test (skips when `mergiraf` absent): a genuine same-key
/// conflict — both sides set the same JSON key to different values — yields a
/// non-empty structured `conflicts` list.
#[test]
fn mergiraf_reports_same_key_conflict() {
    if !mergiraf_available() {
        eprintln!("skipping: mergiraf not on PATH");
        return;
    }
    let base = "{\n  \"a\": 1\n}\n";
    let left = "{\n  \"a\": 2\n}\n";
    let right = "{\n  \"a\": 3\n}\n";
    let out = MergirafDriver::new()
        .merge(&MergeInput {
            base,
            left,
            right,
            path: "config.json",
        })
        .expect("mergiraf merge");
    assert!(
        !out.is_clean(),
        "expected a same-key conflict, got clean:\n{}",
        out.merged
    );
    assert_eq!(out.driver, "mergiraf");
    let c = &out.conflicts[0];
    assert!(
        c.left.iter().any(|l| l.contains('2')),
        "left side of conflict should carry `2`: {c:?}"
    );
    assert!(
        c.right.iter().any(|l| l.contains('3')),
        "right side of conflict should carry `3`: {c:?}"
    );
}
