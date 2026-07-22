//! Multi-**workspace** concurrency stress test (design-doc M2).
//!
//! The companion of `concurrency.rs`: where that test hammers a single ref from
//! many processes, this one proves the **workspace** layer is safe when the
//! user's "twelve agents" each drive their *own* working copy against one shared
//! `.omoplata`. It spawns **12 genuine OS processes** of the built `omo` binary
//! (so cross-process `flock` is exercised, not just in-process threads), each
//! operating in its own workspace directory: every process registers its
//! workspace, then runs a batch of `omo commit` invocations (even-numbered
//! workers also cut an `omo branch`), all appending workspace-scoped ops to the
//! **same** shared op log and mutating the **same** shared registry.
//!
//! After every process has joined it asserts the properties M2 + ADR-0008
//! jointly guarantee:
//!
//! * **(a) no torn/corrupt log** — the whole `oplog.jsonl` parses;
//! * **(b) contiguous, gap-free `seq`** — the set of seqs is exactly
//!   `0..total_ops`, no duplicate and no gap;
//! * **(c) no lost commit** — every process contributed exactly
//!   `COMMITS_PER_WORKER` `Commit` ops, so `WORKERS × COMMITS_PER_WORKER` commit
//!   ops are present;
//! * **(d) consistent registry** — all `WORKERS` workspaces are listed after the
//!   concurrent registrations;
//! * **(e) no cross-workspace clobber** — each workspace's tip in the shared ref
//!   map equals a fresh snapshot of *its own* final working directory.
//!
//! The test is bounded: 12 workers × 4 commits + 6 branches = 54 op-log appends
//! across ~60 short-lived processes, completing in a few seconds.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use omoplata_identity::CommitId;
use omoplata_store::Repository;
use omoplata_work::{snapshot, OpKind, OpLog, WorkspaceRegistry};

/// Number of concurrent worker processes / workspaces.
const WORKERS: usize = 12;
/// `omo commit` invocations each worker performs.
const COMMITS_PER_WORKER: usize = 4;

/// Path to the `omo` binary built for this test run.
fn omo_bin() -> &'static str {
    env!("CARGO_BIN_EXE_omo")
}

/// Run one `omo` invocation against `repo`, panicking on a non-zero exit.
fn run(repo: &Path, args: &[&str]) {
    let status = Command::new(omo_bin())
        .args(args)
        .args(["--repo"])
        .arg(repo)
        .status()
        .unwrap_or_else(|e| panic!("spawn `omo {}`: {e}", args.join(" ")));
    assert!(
        status.success(),
        "`omo {}` failed: {status}",
        args.join(" ")
    );
}

#[test]
fn twelve_workspaces_commit_concurrently_without_clobber() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(&repo_path).expect("mkdir repo");

    // One shared repository every worker contends on.
    let init = Command::new(omo_bin())
        .arg("init")
        .arg(&repo_path)
        .status()
        .expect("spawn `omo init`");
    assert!(init.success(), "`omo init` failed");

    // Each worker gets its own working directory.
    let work_dirs: Vec<PathBuf> = (0..WORKERS)
        .map(|w| dir.path().join(format!("ws{w}")))
        .collect();

    // Spawn WORKERS threads; each drives its own workspace end to end:
    // register -> COMMITS_PER_WORKER commits (rewriting its file each step) ->
    // an optional branch. Registrations mutate the shared registry while other
    // threads' commits mutate the shared op log, so both locked paths contend.
    std::thread::scope(|scope| {
        for (w, work_dir) in work_dirs.iter().enumerate() {
            let repo_path = repo_path.clone();
            let work_dir = work_dir.clone();
            scope.spawn(move || {
                let name = format!("w{w}");
                let dir_arg = work_dir.to_string_lossy().into_owned();
                run(&repo_path, &["workspace", "add", &name, &dir_arg]);

                let file = work_dir.join("work.txt");
                for k in 0..COMMITS_PER_WORKER {
                    // Distinct content per (worker, step) so the final tip is
                    // unambiguous and predictable by re-snapshotting.
                    std::fs::write(&file, format!("workspace {w} step {k}\n"))
                        .expect("write working file");
                    let msg = format!("w{w} commit {k}");
                    run(&repo_path, &["commit", "-m", &msg, "--workspace", &name]);
                }

                if w % 2 == 0 {
                    let branch = format!("feat-w{w}");
                    run(&repo_path, &["branch", &branch, "--workspace", &name]);
                }
            });
        }
    });

    // ---- Assertions -------------------------------------------------------
    let repo = Repository::open(&repo_path).expect("open repo");

    // (a) No torn/corrupt log: the entire file parses (load validates every line).
    let log = OpLog::load(OpLog::path_in(&repo)).expect("op log must parse fully (no torn JSON)");
    let ops = log.operations();

    let commit_ops = COMMITS_PER_WORKER * WORKERS;
    let branch_ops = WORKERS.div_ceil(2); // even workers 0,2,4,... branch once each
    let total_ops = commit_ops + branch_ops;

    // (b) `seq` is contiguous and gap-free: exactly 0..total_ops, no dup, no gap.
    let seqs: BTreeSet<u64> = ops.iter().map(|op| op.seq).collect();
    assert_eq!(
        seqs.len(),
        total_ops,
        "expected {total_ops} distinct seqs, found {} (lost update or duplicate)",
        seqs.len()
    );
    let expected: BTreeSet<u64> = (0..total_ops as u64).collect();
    assert_eq!(
        seqs, expected,
        "seqs are not exactly 0..{total_ops} (gap or dup)"
    );
    for (i, op) in ops.iter().enumerate() {
        assert_eq!(op.seq, i as u64, "op at index {i} has non-monotonic seq");
    }

    // (c) No lost commit: every worker contributed exactly COMMITS_PER_WORKER
    // `Commit` ops, and no more.
    let mut commits = 0usize;
    for w in 0..WORKERS {
        let name = format!("w{w}");
        let mine = ops
            .iter()
            .filter(|op| matches!(&op.kind, OpKind::Commit { workspace, .. } if *workspace == name))
            .count();
        assert_eq!(
            mine, COMMITS_PER_WORKER,
            "workspace {name} landed {mine} commits, expected {COMMITS_PER_WORKER} (lost commit)"
        );
        commits += mine;
    }
    assert_eq!(commits, commit_ops, "total commit ops mismatch");

    // (d) Consistent registry: all WORKERS workspaces are present after the
    // concurrent registrations.
    let reg = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo)).expect("load registry");
    assert_eq!(
        reg.workspaces().len(),
        WORKERS,
        "registry lost a workspace under concurrent `workspace add`"
    );

    // (e) No cross-workspace clobber: each workspace's tip in the shared ref map
    // equals a fresh snapshot of its OWN final working directory.
    let refs = log.refs_now();
    for w in 0..WORKERS {
        let name = format!("w{w}");
        let ws = reg
            .get(&name)
            .unwrap_or_else(|| panic!("workspace {name} missing from registry"));
        let expected_tip = CommitId::new(
            snapshot(&repo, &ws.working_dir)
                .expect("snapshot final working dir")
                .to_string(),
        );
        let tip = refs
            .get(ws.change.as_str())
            .unwrap_or_else(|| panic!("workspace {name} has no tip ref"));
        assert_eq!(
            tip, &expected_tip,
            "workspace {name} tip does not reflect its own last commit (cross-workspace clobber)"
        );
    }

    // Even workers' branches all survived as distinct refs.
    for w in (0..WORKERS).step_by(2) {
        let branch = format!("feat-w{w}");
        assert!(refs.contains_key(&branch), "branch {branch} lost");
    }
}
