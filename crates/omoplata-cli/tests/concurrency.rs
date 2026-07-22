//! Multi-writer concurrency stress test (ADR-0008).
//!
//! This is the executable proof that advisory `flock` locking plus crash-atomic
//! op-log writes make `.omoplata/` safe under real concurrent writers. It spawns
//! **12 genuine OS processes** of the built `omo` binary (not threads sharing one
//! address space, so it exercises cross-process `flock`) and has each one hammer
//! the **same** repository with a batch of `omo ref set` invocations — each a
//! separate process that runs the full `load -> append -> save` op-log cycle.
//!
//! After every process has joined it asserts the four properties the ADR
//! guarantees:
//!
//! * **(a) no torn/corrupt log** — the whole `oplog.jsonl` parses;
//! * **(b) no lost update** — exactly `WRITERS × OPS_PER_WRITER` operations are
//!   present;
//! * **(c) monotonic, gap-free `seq`** — the set of `seq`s is exactly
//!   `0..(WRITERS × OPS_PER_WRITER)`, with no duplicate and no gap;
//! * **(d) every writer's last write survives** — `refs_now()` has one ref per
//!   writer at that writer's final commit.
//!
//! Without the lock, property (b)/(c) fail (lost updates, duplicated/missing
//! `seq`s); without atomic writes, property (a) fails (a half-written final
//! line). The test is deterministic and bounded: 12 × 8 = 96 appends complete in
//! well under a second on a normal machine.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use omoplata_store::Repository;
use omoplata_work::{OpKind, OpLog};

/// Number of concurrent writer processes.
const WRITERS: usize = 12;
/// Op-log appends each writer performs (one `omo ref set` invocation each).
const OPS_PER_WRITER: usize = 8;

/// Path to the `omo` binary built for this test run.
fn omo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_omo")
}

/// Run one `omo ref set <name> <commit> --repo <repo>` invocation, panicking if
/// the child process does not exit successfully.
fn ref_set(repo: &Path, name: &str, commit: &str) {
    let status = Command::new(omo_bin())
        .args(["ref", "set", name, commit, "--repo"])
        .arg(repo)
        .status()
        .expect("spawn `omo ref set`");
    assert!(
        status.success(),
        "`omo ref set {name} {commit}` failed with {status}"
    );
}

#[test]
fn twelve_processes_hammering_refs_lose_no_updates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_path = dir.path().to_path_buf();

    // One shared repository for every writer to contend on.
    let init = Command::new(omo_bin())
        .arg("init")
        .arg(&repo_path)
        .status()
        .expect("spawn `omo init`");
    assert!(init.success(), "`omo init` failed");

    // Spawn WRITERS threads, each driving OPS_PER_WRITER sequential `omo ref set`
    // child processes against the shared repo. At any instant up to WRITERS `omo`
    // processes are live and contending for the repo lock, so this genuinely
    // exercises cross-process `flock`.
    std::thread::scope(|scope| {
        for w in 0..WRITERS {
            let repo_path = repo_path.clone();
            scope.spawn(move || {
                let name = format!("w{w}");
                for k in 0..OPS_PER_WRITER {
                    // A distinct commit per step so the final value is unambiguous.
                    let commit = format!("sha256:{w:02x}{k:02x}");
                    ref_set(&repo_path, &name, &commit);
                }
            });
        }
    });

    // ---- Assert on the final op log ---------------------------------------
    let repo = Repository::open(&repo_path).expect("open repo");
    let log_path = OpLog::path_in(&repo);

    // (a) No torn/corrupt log: the entire file parses. `OpLog::load` validates
    // every JSON line, so a successful load *is* the "no torn line" assertion.
    let log = OpLog::load(&log_path).expect("op log must parse fully (no torn JSON line)");
    let ops = log.operations();

    let total = WRITERS * OPS_PER_WRITER;

    // (b) No lost update: exactly WRITERS × OPS_PER_WRITER operations landed.
    assert_eq!(
        ops.len(),
        total,
        "expected {total} operations (no lost update), found {}",
        ops.len()
    );

    // (c) `seq` is monotonic and gap-free: the set of seqs is exactly 0..total,
    // with no duplicate (a duplicate would shrink the set below `total`) and no
    // gap (a gap would push the max seq past total-1).
    let seqs: BTreeSet<u64> = ops.iter().map(|op| op.seq).collect();
    assert_eq!(
        seqs.len(),
        total,
        "duplicate seqs present: {} distinct of {total}",
        seqs.len()
    );
    let expected: BTreeSet<u64> = (0..total as u64).collect();
    assert_eq!(
        seqs, expected,
        "seqs are not exactly 0..{total} (gap or dup)"
    );
    // Belt and suspenders: they are also strictly increasing in file order.
    for (i, op) in ops.iter().enumerate() {
        assert_eq!(op.seq, i as u64, "op at index {i} has non-monotonic seq");
        assert!(
            matches!(op.kind, OpKind::SetRef { .. }),
            "unexpected op kind at {i}: {:?}",
            op.kind
        );
    }

    // (d) Every writer's last write survives: one ref per writer, each at that
    // writer's final commit `sha256:{w:02x}{OPS_PER_WRITER-1:02x}`.
    let refs = log.refs_now();
    assert_eq!(refs.len(), WRITERS, "expected one ref per writer");
    for w in 0..WRITERS {
        let name = format!("w{w}");
        let last = format!("sha256:{w:02x}{:02x}", OPS_PER_WRITER - 1);
        let got = refs
            .get(&name)
            .unwrap_or_else(|| panic!("writer {name} lost its ref entirely"));
        assert_eq!(
            got.as_str(),
            last,
            "writer {name} final ref mismatch (a lost update to its own ref)"
        );
    }
}
