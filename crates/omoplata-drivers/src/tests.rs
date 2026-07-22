//! Unit tests for the Tier-2 drivers, focused on the structural-vs-line win.

use crate::{select_driver, LineDriver, MergeDriver, MergeInput, RustStructuralDriver};

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

/// `select_driver` routes by extension.
#[test]
fn select_driver_routes_by_extension() {
    assert_eq!(select_driver("x.rs").name(), "rust-structural");
    assert_eq!(select_driver("src/lib.rs").name(), "rust-structural");
    assert_eq!(select_driver("x.txt").name(), "line");
    assert_eq!(select_driver("README").name(), "line");
    assert_eq!(select_driver("archive.rs.bak").name(), "line");
}
