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

// ---------------------------------------------------------------------------
// Container recursion: definition granularity inside impl/mod/trait blocks
// ---------------------------------------------------------------------------

const IMPL_BASE: &str = "struct Q { n: usize }\n\nimpl Q {\n    fn len(&self) -> usize {\n        self.n\n    }\n\n    fn pop(&mut self) -> usize {\n        self.n -= 1;\n        self.n\n    }\n}\n";

/// Two sides adding *different* methods to the same impl block — at different
/// anchors — merge cleanly at member granularity (previously an interior line
/// merge that could silently interleave).
#[test]
fn impl_disjoint_method_adds_merge_clean() {
    let left = IMPL_BASE.replace(
        "    fn pop(&mut self)",
        "    fn is_empty(&self) -> bool {\n        self.n == 0\n    }\n\n    fn pop(&mut self)",
    );
    let right = IMPL_BASE.replace(
        "        self.n\n    }\n}",
        "        self.n\n    }\n\n    fn clear(&mut self) {\n        self.n = 0;\n    }\n}",
    );
    let out = structural(IMPL_BASE, &left, &right);
    assert!(out.is_clean(), "expected clean merge, got: {}", out.merged);
    assert!(out.merged.contains("fn is_empty") && out.merged.contains("fn clear"));
    // Exactly one impl block in the output.
    assert_eq!(out.merged.matches("impl Q").count(), 1);
}

/// Duplicate work: both sides independently add a method with the same name,
/// different bodies, at different anchors. A line merge compiles both copies
/// in silently; member granularity must surface an honest conflict.
#[test]
fn impl_same_name_add_is_honest_conflict() {
    let left = IMPL_BASE.replace(
        "    fn pop(&mut self)",
        "    fn is_empty(&self) -> bool {\n        self.n == 0\n    }\n\n    fn pop(&mut self)",
    );
    let right = IMPL_BASE.replace(
        "        self.n\n    }\n}",
        "        self.n\n    }\n\n    fn is_empty(&self) -> bool {\n        self.len() == 0\n    }\n}",
    );
    let out = structural(IMPL_BASE, &left, &right);
    assert!(!out.is_clean(), "duplicate add must not merge silently");
    assert_eq!(out.conflicts.len(), 1);
    assert!(out.merged.contains("<<<<<<<"), "conflict must be rendered");
}

/// Both sides add an *identical* method: include it once, clean.
#[test]
fn impl_same_name_identical_add_dedupes() {
    let addition = "    fn is_empty(&self) -> bool {\n        self.n == 0\n    }\n\n";
    let left = IMPL_BASE.replace("    fn pop", &format!("{addition}    fn pop"));
    let right = IMPL_BASE.replace("    fn pop", &format!("{addition}    fn pop"));
    let out = structural(IMPL_BASE, &left, &right);
    assert!(out.is_clean());
    assert_eq!(out.merged.matches("fn is_empty").count(), 1);
}

/// A container whose header changed on one side falls back to the interior
/// line merge and still merges when the edits are line-disjoint.
#[test]
fn impl_header_change_falls_back_to_line_interior() {
    let left = IMPL_BASE.replace("impl Q {", "impl Q {\n    // note\n");
    let right = IMPL_BASE.replace("self.n -= 1;", "self.n = self.n.saturating_sub(1);");
    let out = structural(IMPL_BASE, &left, &right);
    assert!(out.is_clean(), "line-disjoint edits should merge: {}", out.merged);
}

// ---------------------------------------------------------------------------
// Conflicts as values: ride-through and honest nesting (§5.4, P3)
// ---------------------------------------------------------------------------

const CV_BASE: &str = "fn alpha() -> u32 {\n    1\n}\n\nfn beta() -> u32 {\n    2\n}\n";

/// A left side carrying an unresolved conflict value in `alpha`.
fn cv_left() -> String {
    CV_BASE.replace(
        "fn alpha() -> u32 {\n    1\n}",
        "fn alpha() -> u32 {\n<<<<<<< left\n    10\n=======\n    11\n>>>>>>> right\n}",
    )
}

/// The other side edits a *different* definition: the conflict value rides
/// through byte-identically, the other edit merges, and the carried value is
/// reported out-of-band — the merge is mergeable, not failed.
#[test]
fn conflict_value_rides_through_disjoint_merge() {
    let left = cv_left();
    let right = CV_BASE.replace("    2\n", "    22\n");
    let out = structural(CV_BASE, &left, &right);
    assert_eq!(out.driver, "rust-structural", "must not degrade to line");
    assert!(out.conflicts.is_empty(), "no NEW conflicts: {}", out.merged);
    assert_eq!(out.carried.len(), 1, "the value must be carried");
    assert!(out.is_mergeable() && !out.is_clean());
    // The carried block is preserved verbatim and the disjoint edit landed.
    assert!(out.merged.contains("<<<<<<< left\n    10\n=======\n    11\n>>>>>>> right"));
    assert!(out.merged.contains("    22\n"));
}

/// The other side edits the SAME definition that carries the value: nest
/// honestly as a fresh conflict — both full texts survive, nothing is picked.
#[test]
fn conflict_value_nests_when_both_touch() {
    let left = cv_left();
    let right = CV_BASE.replace("fn alpha() -> u32 {\n    1\n}", "fn alpha() -> u32 {\n    99\n}");
    let out = structural(CV_BASE, &left, &right);
    assert_eq!(out.conflicts.len(), 1, "must be a fresh conflict");
    assert!(out.merged.contains("    99"), "right's text must survive");
    assert!(out.merged.contains("    10"), "carried left variant must survive");
}

/// A side that differs from a conflict-carrying base is a resolution: it wins
/// and the term collapses (no carried values remain).
#[test]
fn resolution_collapses_carried_conflict() {
    let base = cv_left(); // base itself carries the value
    let left = CV_BASE.replace("fn alpha() -> u32 {\n    1\n}", "fn alpha() -> u32 {\n    10\n}");
    let right = base.clone(); // right leaves the conflict untouched
    let out = structural(&base, &left, &right);
    assert!(out.is_clean(), "resolution should collapse: {}", out.merged);
    assert!(out.merged.contains("    10") && !out.merged.contains("<<<<<<<"));
}

/// Conflict values are queryable, pinned to their containing definition.
#[test]
fn conflict_values_are_queryable() {
    let vals = crate::rust::conflict_values(&cv_left()).expect("well-formed");
    assert_eq!(vals.len(), 1);
    assert_eq!(vals[0].definition.as_deref(), Some("alpha"));
    assert_eq!(vals[0].line, 2);
    assert_eq!(vals[0].left, vec!["    10".to_owned()]);
}

/// Malformed marker structure (unterminated block) bails to the line driver
/// rather than guessing.
#[test]
fn malformed_markers_fall_back_to_line() {
    let left = CV_BASE.replace("    1\n", "<<<<<<< left\n    1\n");
    let out = structural(CV_BASE, &left, CV_BASE);
    assert_eq!(out.driver, "line");
}

/// A member-scoped conflict (as rendered by container recursion, with the
/// start marker indented) must also ride through a later disjoint merge —
/// end-to-end: produce the conflict via a merge, then merge again on top.
#[test]
fn member_conflict_value_rides_through_next_merge() {
    // Round A: duplicate-work conflict inside the impl block.
    let left = IMPL_BASE.replace(
        "    fn pop(&mut self)",
        "    fn is_empty(&self) -> bool {\n        self.n == 0\n    }\n\n    fn pop(&mut self)",
    );
    let right = IMPL_BASE.replace(
        "        self.n\n    }\n}",
        "        self.n\n    }\n\n    fn is_empty(&self) -> bool {\n        self.len() == 0\n    }\n}",
    );
    let a = structural(IMPL_BASE, &left, &right);
    assert_eq!(a.conflicts.len(), 1);

    // Round B: the conflicted output is the new left; the new right makes a
    // disjoint edit to a different top-level item.
    let right_b = IMPL_BASE.replace("struct Q { n: usize }", "struct Q { n: u64 }");
    let b = structural(IMPL_BASE, &a.merged, &right_b);
    assert_eq!(b.driver, "rust-structural", "must not degrade: {}", b.merged);
    assert!(b.conflicts.is_empty(), "no new conflicts: {}", b.merged);
    assert_eq!(b.carried.len(), 1, "member conflict must be carried");
    assert!(b.merged.contains("n: u64"), "disjoint edit must land");
    assert!(b.merged.contains("<<<<<<<"), "value preserved");
}
