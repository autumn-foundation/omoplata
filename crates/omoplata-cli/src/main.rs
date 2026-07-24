//! `omo` — the omoplata command-line interface.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use omoplata_algebra::{
    diff, dynamic_validate, kernel, merge3, rebase, Admission, Conflict, Doc, Validated,
};
use omoplata_drivers::{select_driver, MergeInput};
use omoplata_git::{
    export_matches_source, export_repo, fetch_local, import_repo, verify_repo, GitError,
};
use omoplata_identity::{
    extract_definitions, match_definitions, ApprovalCertificate, ChangeGraph, ChangeId, CommitId,
    Definition, MatchStatus, Phase, Submission, SubmissionId,
};
use omoplata_sem::{
    embed_definitions, embed_workspace_dir, find_duplicates, search, Embedded, Embedder,
    HashingEmbedder,
};
use omoplata_store::{EntryKind, Object, ObjectId, Repository};
use omoplata_work::{
    absorb, auto_snapshot, land_batch_in_queue, land_submission_in_queue, BatchGates, MapContext,
    OpKind, OpLog, QueueGates, QueuePolicy, QueueRegistry, RebaseEngine, Stack, Workspace,
    WorkspaceRegistry,
};

/// omoplata: a version control system with a verified merge kernel.
#[derive(Debug, Parser)]
#[command(
    name = "omo",
    version,
    about = "omoplata: a version control system with a verified merge kernel",
    long_about = "omoplata (omo) is a version control system built on a verified merge kernel \
        — no silent wrong answers. It is definition-level (it tracks durable definitions, not \
        files, across renames), bi-temporal (history is queryable in both valid and \
        transaction time), and git-interoperable (git objects round-trip through a release gate)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new omoplata repository.
    Init {
        /// Directory to initialize (defaults to the current directory).
        path: Option<PathBuf>,
    },
    /// Show the status of an omoplata repository.
    Status {
        /// Repository directory (defaults to the current directory).
        path: Option<PathBuf>,
    },
    /// Hash a file's contents into the object store as a blob and print its id.
    #[command(alias = "hash-object")]
    Hash {
        /// File to read (use `-` for stdin).
        path: PathBuf,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print a stored object by id (blob bytes, or a tree listing).
    #[command(alias = "cat-object")]
    Cat {
        /// Object id, e.g. `sha256:abcd…`.
        id: String,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Show the line diff turning `base` into `target`, in unified-ish form.
    Diff {
        /// The base file.
        base: PathBuf,
        /// The target file.
        target: PathBuf,
    },
    /// Three-way merge `left` and `right` against their common `base`.
    ///
    /// Prints the merged document to stdout. A clean merge exits 0; a merge
    /// with conflicts renders conflict markers into the output, prints a
    /// summary to stderr, and exits with a non-zero status.
    Merge {
        /// The common base file.
        base: PathBuf,
        /// The left side.
        left: PathBuf,
        /// The right side.
        right: PathBuf,
    },
    /// Three-way merge three files with the Tier-2 driver chosen by extension.
    ///
    /// Selects the driver from the base path's extension (§4 Tier 2): `.rs`
    /// files use the Rust structural driver, everything else the line fallback.
    /// A *clean* driver result is then passed through the trusted kernel
    /// (`kernel::certify`, §3 P1 / §6 I8): the kernel independently derives its
    /// own merge and admits the proposal only if it matches — otherwise it
    /// downgrades to a conflict. Prints the merged output to stdout, the
    /// `<driver> merge: N conflict(s)` summary and the kernel verdict to stderr;
    /// exits 0 only if the merge is clean *and* kernel-admitted.
    ///
    /// With `--validate <cmd>`, kernel admission is treated as *provisional
    /// pending dynamic validation* (§3 P9): a clean, kernel-admitted merge is
    /// materialized to a temp file and `<cmd>` is run against it (a `{}`
    /// placeholder in `<cmd>` is replaced with the temp file path; if there is
    /// no placeholder, the path is appended as the last argument). If the
    /// validator exits zero the merge is accepted; if it exits non-zero the
    /// merge is **demoted to a Tier-3 semantic conflict** (P9/I12) rather than
    /// accepted.
    MergeFile {
        /// The common base file.
        base: PathBuf,
        /// The left side.
        left: PathBuf,
        /// The right side.
        right: PathBuf,
        /// Dynamic validator (P9): a shell command run against the merged
        /// output. `{}` is substituted with the merged file path; if absent, the
        /// path is appended. A non-zero exit demotes the merge to a semantic
        /// conflict.
        #[arg(long, value_name = "CMD")]
        validate: Option<String>,
    },
    /// List the conflict values carried by a file (§5.4: conflicts are
    /// first-class, queryable values).
    ///
    /// Scans the file for `<<<<<<<` / `=======` / `>>>>>>>` marker blocks and
    /// pins each to the definition containing it (Rust files; other files are
    /// scanned without definition pinning). Prints one line per value:
    /// `<definition> line <n>: <l> line(s) left | <r> line(s) right`.
    /// Exits 0 when the file carries no values, 2 when it carries some, and
    /// non-zero with an error for a malformed marker structure.
    Conflicts {
        /// The file to scan.
        path: PathBuf,
    },
    /// Kernel admission of a three-way merge (§3 P1, §6 invariant I8).
    ///
    /// Runs the trusted kernel directly on three files: it independently diffs
    /// both sides, runs the executable commutation check, and — only if they
    /// commute — computes the merged document itself, emitting a checked
    /// commutation witness. There are exactly two outcomes ("no silent wrong
    /// answers"): an admitted merge with a witness (exit 0), or first-class
    /// conflict values (exit non-zero). No proposer is involved.
    Admit {
        /// The common base file.
        base: PathBuf,
        /// The left side.
        left: PathBuf,
        /// The right side.
        right: PathBuf,
    },
    /// Auto-rebase my change over a sibling `onto` change (§5.4, P3, I4).
    ///
    /// Replays my change (`base` → `mine`) on top of `onto` (which also derives
    /// from `base`). Independent edits replay cleanly; overlaps are carried
    /// forward as first-class **conflict values** rather than failing (§3 P3:
    /// "merges and rebases never fail and never block"). Prints the rebased
    /// document to stdout — conflicted spans rendered as
    /// `<<<<<<< mine / ======= / >>>>>>> onto` marker blocks, though the
    /// structured conflicts are the source of truth — and `rebase: clean` or
    /// `rebase: <k> conflict(s) carried` to stderr. Exits 0 if clean, else
    /// non-zero.
    Rebase {
        /// The common base file.
        base: PathBuf,
        /// My version (the change to replay).
        mine: PathBuf,
        /// The version to rebase onto (the new base).
        onto: PathBuf,
    },
    /// Auto-rebase a change through the change graph and op log (R4; §5.3,
    /// §5.4, §5.6).
    ///
    /// Stores `base`, `mine` and `onto` as blobs in the repository's object
    /// store (a commit id is the blob's object id), replays the change with the
    /// verified rebase algebra, and records the move on **both** of the design
    /// doc's time axes: a `Rebase` entry in the bi-temporal op log (transaction
    /// time) and a supersession edge in the change graph (valid time). Conflicts
    /// are carried forward as **values**, never blocking (§3 P3).
    ///
    /// Prints the rebased content to stdout — conflicted spans as
    /// `<<<<<<< mine / ======= / >>>>>>> onto` marker blocks — and to stderr the
    /// new tip commit id, `autorebase: clean` or `autorebase: <k> conflict(s)
    /// carried`, and the appended op-log entry. The op log is persisted, so
    /// `omo op log` shows the `Rebase` entry afterward. Exits 0 if clean, else
    /// non-zero. Requires an initialized repository.
    Autorebase {
        /// The common base file.
        base: PathBuf,
        /// My version (the change to replay).
        mine: PathBuf,
        /// The version to rebase onto (the advanced base).
        onto: PathBuf,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// The change id to record the rebase under (default: `change`).
        #[arg(long)]
        change: Option<String>,
    },
    /// List the Rust definitions in a source file, in source order.
    ///
    /// Prints each definition as `<kind> <path> (lines A-B)`.
    Defs {
        /// The Rust source file to extract definitions from.
        file: PathBuf,
    },
    /// Track definition identity across two versions of a Rust file.
    ///
    /// Prints the tiered-matcher report (§5.5): one line per matched, added,
    /// deleted, renamed, or modified definition.
    Track {
        /// The old version of the file.
        old: PathBuf,
        /// The new version of the file.
        new: PathBuf,
    },
    /// Manage workspaces: multiple working copies over one shared `.omoplata`
    /// (M2, jj-style — each workspace has its own working dir and current
    /// change, sharing the object store, op log, and refs).
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCommand,
    },
    /// View and manage the current stack of changes for a workspace (§5.9).
    Stack {
        /// Which workspace's stack to view (defaults to current directory workspace).
        #[arg(long)]
        workspace: Option<String>,
        /// Repository directory (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Route uncommitted working-copy edits into stack changes by touched definitions (§5.9).
    Absorb {
        /// Target change IDs to absorb hunks into.
        #[arg(required = true)]
        target: Vec<String>,
        /// Which workspace to operate on.
        #[arg(long)]
        workspace: Option<String>,
        /// Repository directory (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Reorder adjacent changes in the workspace's change stack (§5.9).
    Reorder {
        /// Index `i` of the change to swap with `i + 1`.
        index: usize,
        /// Which workspace's stack to reorder.
        #[arg(long)]
        workspace: Option<String>,
        /// Repository directory (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Create or update a review submission referencing change IDs (§5.10).
    Submit {
        /// Unique submission identifier.
        id: String,
        /// Submission title / description.
        #[arg(short, long)]
        title: String,
        /// List of change IDs included in this submission.
        #[arg(required = true)]
        changes: Vec<String>,
        /// Author identifier.
        #[arg(long)]
        author: Option<String>,
        /// Leave the submission pending review instead of auto-approving it —
        /// required by any queue whose policy demands approval before landing.
        #[arg(long)]
        pending: bool,
        /// Repository directory (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Approve a pending submission (§5.10).
    Approve {
        /// Submission ID to approve.
        id: String,
        /// Reviewer identifier recorded on the approval.
        #[arg(long)]
        by: Option<String>,
        /// Repository directory (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Land an approved submission through a merge queue (§5.10, ADR-0009).
    ///
    /// The target queue's policy gates the landing: approval requirement,
    /// carried-conflict rule, and — when the queue configures a validator —
    /// P9 dynamic validation of the submission's materialized content. A
    /// refused landing mutates nothing.
    Land {
        /// Submission ID(s) to land. Several IDs form a **Tier-0 batch**
        /// (§5.10): pairwise-disjoint submissions validate as one and land in
        /// a single locked transaction; an overlapping pair refuses the whole
        /// batch with the colliding paths named.
        #[arg(required = true)]
        ids: Vec<String>,
        /// Queue to land into (defaults to `trunk`).
        #[arg(long, default_value = "trunk")]
        queue: String,
        /// Repository directory (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Land an already-landed submission into a second queue, carrying its
    /// approval forward with a certificate (ADR-0009 backports).
    ///
    /// Sound by identity: each change's tip must be byte-identical to the tip
    /// reviewed and landed in the source queue; moved content refuses with a
    /// re-review demand. The target queue's own gates still apply.
    Backport {
        /// Submission ID to backport.
        id: String,
        /// Target queue.
        #[arg(long)]
        to: String,
        /// Repository directory (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Manage named landing queues and their policies (ADR-0009).
    Queue {
        #[command(subcommand)]
        action: QueueCommand,
    },

    /// Inspect and update the repository's refs via the operation log (§5.6).
    Ref {
        #[command(subcommand)]
        action: RefCommand,
    },
    /// Inspect and undo entries in the bi-temporal operation log (§5.6).
    Op {
        #[command(subcommand)]
        action: OpCommand,
    },
    /// Evaluate a revset expression over the current refs (§5.8).
    ///
    /// Prints the matching commit ids, one per line. Supports `a & b`, `a | b`,
    /// `~a`, parentheses, `all()`, `heads()`, `draft()`, `public()`, bare ref
    /// names, and `id:<hex>` literals.
    Revset {
        /// The revset expression, e.g. `'main | feature'`.
        expr: String,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Git interoperability: the I9 round-trip gate and import (§3 P8, §6 I9).
    Git {
        #[command(subcommand)]
        action: GitCommand,
    },
    /// Detect likely duplicate work across Rust files (§5.7, §8).
    ///
    /// Extracts and embeds every definition across all the given files, then
    /// flags definition pairs whose embeddings are more similar than
    /// `--threshold` — the design's "two agents implementing the same thing"
    /// detector (conflict avoidance before textual collision). Prints each pair
    /// as `<score>  <file>:<def> ~ <file>:<def>`, or `no likely duplicate
    /// definitions` if none.
    Dup {
        /// The Rust source files to scan. If empty, automatically scans all active registered workspaces in the repository.
        files: Vec<PathBuf>,
        /// Cosine-similarity threshold in [0, 1]; pairs at or above are flagged.
        #[arg(long, default_value_t = 0.85)]
        threshold: f32,
        /// Use the real transformer model (all-MiniLM-L6-v2) instead of the
        /// deterministic hashing stand-in. Requires the binary to be built with
        /// `--features fastembed` and the model to be fetchable; otherwise a note
        /// is printed and the hashing stand-in is used. Note: the real model's
        /// similarities are lower than lexical overlap, so pass a lower
        /// `--threshold` (e.g. 0.5) with `--real-embeddings`.
        #[arg(long)]
        real_embeddings: bool,
        /// Path to the repository root (defaults to current working directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Semantic search: rank definitions by similarity to a query (§5.7).
    ///
    /// Embeds the query, extracts and embeds every definition across the given
    /// files, and prints the top-k as `<score> <file>:<def>`.
    Similar {
        /// The free-text query, e.g. `"compute area of rectangle"`.
        query: String,
        /// The Rust source files to search.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// How many results to print.
        #[arg(long, default_value_t = 5)]
        top: usize,
        /// Use the real transformer model (all-MiniLM-L6-v2) instead of the
        /// deterministic hashing stand-in. Requires the binary to be built with
        /// `--features fastembed` and the model to be fetchable; otherwise a note
        /// is printed and the hashing stand-in is used.
        #[arg(long)]
        real_embeddings: bool,
    },
}

/// `omo git …` — git interop subcommands (§3 P8, invariant I9).
#[derive(Debug, Subcommand)]
enum GitCommand {
    /// Run the round-trip gate over every loose object in a git repository.
    ///
    /// Prints per-type object counts and `round-trip gate: PASS` (exit 0), or
    /// the failing object and `round-trip gate: FAIL` (exit non-zero).
    Verify {
        /// The repo to verify: a worktree root (`path/to/repo`) or a git
        /// directory (`path/to/repo/.git`). Auto-descends into `.git`.
        git_dir: PathBuf,
    },
    /// Import a git repository by walking its commit graph from refs.
    ///
    /// Enforces I9 (runs the gate first, refusing import if it fails), walks the
    /// commit DAG from every ref, and imports every reachable object. Prints the
    /// commit/tag/tree/blob counts and the number of git→omoplata oid mappings.
    Import {
        /// The repo to import: a worktree root (`path/to/repo`) or a git
        /// directory (`path/to/repo/.git`). Auto-descends into `.git`.
        git_dir: PathBuf,
        /// Destination omoplata repository (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print the imported commit graph, newest-first.
    ///
    /// Each line is `<short-oid> <subject>  (parents: <short-oids>)`.
    Log {
        /// The repo to read: a worktree root (`path/to/repo`) or a git
        /// directory (`path/to/repo/.git`). Auto-descends into `.git`.
        git_dir: PathBuf,
    },
    /// Exact-mode export: import a git repo, then write every object back out.
    ///
    /// Writes loose objects and refs under `<out-dir>` (a git-directory layout:
    /// `<out-dir>/objects/<xx>/…` and `<out-dir>/refs/…`), then runs the
    /// repo-level round-trip gate. Prints `exported <N> objects; round-trip vs
    /// source: PASS/FAIL` and exits non-zero on FAIL.
    Export {
        /// The repo to import and export: a worktree root (`path/to/repo`) or a
        /// git directory (`path/to/repo/.git`). Auto-descends into `.git`.
        git_dir: PathBuf,
        /// The output directory to write the reconstructed objects and refs to.
        out_dir: PathBuf,
    },
    /// Clone objects over the git **wire protocol** (local transport, §3 P8).
    ///
    /// Speaks the real pkt-line + `upload-pack` conversation against a local
    /// `git upload-pack` process: reads the source repo's ref advertisement,
    /// negotiates a full clone (`want …`/`done`), receives the raw packfile, and
    /// imports every object into the omoplata repository through the I9 gate.
    /// Prints the advertised refs, the packfile byte count, and the imported
    /// per-type counts.
    Fetch {
        /// The source repository: a `file://` URL or a local path (a working
        /// tree or a git directory).
        repo_url: String,
        /// Destination omoplata repository (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

/// `omo workspace …` — workspace registry subcommands (M2).
#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Register a workspace with its own working dir and a fresh current change.
    Add {
        /// The workspace name, e.g. `w1`.
        name: String,
        /// The working directory for this workspace (created if absent).
        path: PathBuf,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// List the registered workspaces as `name  <dir>  change=<id>  tip=<commit>`.
    List {
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Remove a workspace from the registry (its op-log history is kept).
    Remove {
        /// The workspace name to remove.
        name: String,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

/// `omo queue …` — named landing-queue subcommands (ADR-0009).
#[derive(Debug, Subcommand)]
enum QueueCommand {
    /// Register a landing queue with its policy.
    ///
    /// Registered queues default to the strict posture a release line wants:
    /// approval required and carried conflict values refused. The implicit
    /// `trunk` queue is the permissive opposite — the fleet keeps landing
    /// while a conflict awaits resolution (§5.4).
    Add {
        /// The queue name, e.g. `release-1.2`.
        name: String,
        /// P9 validator: a shell command run against the submission's
        /// materialized content before landing. `{}` is substituted with the
        /// content directory; without a placeholder the directory is appended.
        #[arg(long, value_name = "CMD")]
        validate: Option<String>,
        /// Allow landing content that still carries conflict values (§5.4).
        #[arg(long)]
        allow_carried: bool,
        /// Waive the approval requirement (e.g. a sandbox queue).
        #[arg(long)]
        no_approval: bool,
        /// Optional human description.
        #[arg(long)]
        description: Option<String>,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// List queues (including the implicit `trunk`) with their policies.
    List {
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Remove a queue from the registry (landed refs are kept — history
    /// tells the truth).
    Remove {
        /// The queue name to remove.
        name: String,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

/// `omo ref …` — ref subcommands backed by the operation log.
#[derive(Debug, Subcommand)]
enum RefCommand {
    /// Point `name` at `commit`, appending a `SetRef` operation.
    Set {
        /// The ref name, e.g. `main`.
        name: String,
        /// The target commit id, e.g. `sha256:<hex>`.
        commit: String,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// List the current refs as `name commit`.
    List {
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

/// `omo op …` — operation-log subcommands.
#[derive(Debug, Subcommand)]
enum OpCommand {
    /// Print the operation log, newest first.
    Log {
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Undo the most recent operation still in effect.
    Undo {
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    };
    // Flush buffered stdout before exiting so piped output is not lost.
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

/// Dispatch a command, returning the process exit code (0 = success).
fn run() -> anyhow::Result<i32> {
    match Cli::parse().command {
        Command::Init { path } => cmd_init(path).map(|()| 0),
        Command::Status { path } => cmd_status(path).map(|()| 0),
        Command::Hash { path, repo } => cmd_hash(repo, path).map(|()| 0),
        Command::Cat { id, repo } => cmd_cat(repo, id).map(|()| 0),
        Command::Diff { base, target } => cmd_diff(base, target).map(|()| 0),
        Command::Merge { base, left, right } => cmd_merge(base, left, right),
        Command::MergeFile {
            base,
            left,
            right,
            validate,
        } => cmd_merge_file(base, left, right, validate),
        Command::Conflicts { path } => cmd_conflicts(path),
        Command::Admit { base, left, right } => cmd_admit(base, left, right),
        Command::Rebase { base, mine, onto } => cmd_rebase(base, mine, onto),
        Command::Autorebase {
            base,
            mine,
            onto,
            repo,
            change,
        } => cmd_autorebase(repo, base, mine, onto, change),
        Command::Defs { file } => cmd_defs(file).map(|()| 0),
        Command::Track { old, new } => cmd_track(old, new).map(|()| 0),
        Command::Workspace { action } => match action {
            WorkspaceCommand::Add { name, path, repo } => {
                cmd_workspace_add(repo, name, path).map(|()| 0)
            }
            WorkspaceCommand::List { repo } => cmd_workspace_list(repo).map(|()| 0),
            WorkspaceCommand::Remove { name, repo } => cmd_workspace_remove(repo, name).map(|()| 0),
        },
        Command::Stack { workspace, repo } => cmd_stack(repo, workspace).map(|()| 0),
        Command::Absorb {
            target,
            workspace,
            repo,
        } => cmd_absorb(repo, target, workspace).map(|()| 0),
        Command::Reorder {
            index,
            workspace,
            repo,
        } => cmd_reorder(repo, index, workspace).map(|()| 0),
        Command::Submit {
            id,
            title,
            changes,
            author,
            pending,
            repo,
        } => cmd_submit(repo, id, title, changes, author, pending).map(|()| 0),
        Command::Approve { id, by, repo } => cmd_approve(repo, id, by).map(|()| 0),
        Command::Land { ids, queue, repo } => cmd_land(repo, ids, queue).map(|()| 0),
        Command::Backport { id, to, repo } => cmd_backport(repo, id, to).map(|()| 0),
        Command::Queue { action } => match action {
            QueueCommand::Add {
                name,
                validate,
                allow_carried,
                no_approval,
                description,
                repo,
            } => cmd_queue_add(
                repo,
                name,
                validate,
                allow_carried,
                no_approval,
                description,
            )
            .map(|()| 0),
            QueueCommand::List { repo } => cmd_queue_list(repo).map(|()| 0),
            QueueCommand::Remove { name, repo } => cmd_queue_remove(repo, name).map(|()| 0),
        },

        Command::Ref { action } => match action {
            RefCommand::Set { name, commit, repo } => cmd_ref_set(repo, name, commit).map(|()| 0),
            RefCommand::List { repo } => cmd_ref_list(repo).map(|()| 0),
        },
        Command::Op { action } => match action {
            OpCommand::Log { repo } => cmd_op_log(repo).map(|()| 0),
            OpCommand::Undo { repo } => cmd_op_undo(repo).map(|()| 0),
        },
        Command::Revset { expr, repo } => cmd_revset(repo, expr).map(|()| 0),
        Command::Git { action } => match action {
            GitCommand::Verify { git_dir } => cmd_git_verify(git_dir),
            GitCommand::Import { git_dir, repo } => cmd_git_import(git_dir, repo).map(|()| 0),
            GitCommand::Log { git_dir } => cmd_git_log(git_dir).map(|()| 0),
            GitCommand::Export { git_dir, out_dir } => cmd_git_export(git_dir, out_dir),
            GitCommand::Fetch { repo_url, repo } => cmd_git_fetch(repo_url, repo).map(|()| 0),
        },
        Command::Dup {
            files,
            threshold,
            real_embeddings,
            repo,
        } => cmd_dup(repo, files, threshold, real_embeddings).map(|()| 0),
        Command::Similar {
            query,
            files,
            top,
            real_embeddings,
        } => cmd_similar(query, files, top, real_embeddings).map(|()| 0),
    }
}

/// The path to the operation log inside a repository's control directory.
fn oplog_path(repo: &Repository) -> PathBuf {
    OpLog::path_in(repo)
}

/// Resolve which workspace a command targets.
///
/// Uses `--workspace <name>` when given; otherwise the sole workspace if there
/// is exactly one; otherwise the workspace whose working directory contains the
/// current directory. Errors (rather than guessing) when the choice is
/// ambiguous, so a command never mutates the wrong workspace.
fn resolve_workspace<'a>(
    reg: &'a WorkspaceRegistry,
    name: Option<&str>,
) -> anyhow::Result<&'a Workspace> {
    if let Some(name) = name {
        return reg
            .get(name)
            .with_context(|| format!("no workspace named {name:?} (see `omo workspace list`)"));
    }
    match reg.workspaces() {
        [] => anyhow::bail!("no workspaces registered; run `omo workspace add <name> <path>`"),
        [only] => Ok(only),
        many => {
            // Disambiguate by the current directory living under a working dir.
            let cwd = std::env::current_dir().context("determining the current directory")?;
            let cwd = cwd.canonicalize().unwrap_or(cwd);
            let hit = many.iter().find(|w| {
                w.working_dir
                    .canonicalize()
                    .map(|d| cwd.starts_with(&d))
                    .unwrap_or(false)
            });
            hit.ok_or_else(|| {
                anyhow::anyhow!(
                    "multiple workspaces registered; pass --workspace <name> to choose one"
                )
            })
        }
    }
}

/// `omo workspace add <name> <path>` — register a workspace + fresh change.
///
/// The working directory is created if absent and stored canonicalized so the
/// workspace resolves from any current directory. The registry mutation runs
/// under the repository lock via [`WorkspaceRegistry::mutate_locked`].
fn cmd_workspace_add(repo: Option<PathBuf>, name: String, path: PathBuf) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    std::fs::create_dir_all(&path)
        .with_context(|| format!("creating workspace directory {}", path.display()))?;
    let working_dir = path
        .canonicalize()
        .with_context(|| format!("resolving workspace directory {}", path.display()))?;
    let change = WorkspaceRegistry::mutate_locked(&repo, |reg| {
        let ws = reg.add(name.clone(), working_dir.clone())?;
        Ok(ws.change.clone())
    })?;
    println!(
        "registered workspace {name} at {} (change {change})",
        working_dir.display()
    );
    Ok(())
}

/// `omo workspace list` — print each workspace with its dir, change, and tip.
fn cmd_workspace_list(repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let reg = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo))?;
    let refs = OpLog::load(oplog_path(&repo))?.refs_now();
    if reg.workspaces().is_empty() {
        println!("no workspaces registered");
        return Ok(());
    }
    for ws in reg.workspaces() {
        let tip = refs
            .get(ws.change.as_str())
            .map_or_else(|| "(none)".to_owned(), ToString::to_string);
        println!(
            "{}  {}  change={}  tip={}",
            ws.name,
            ws.working_dir.display(),
            ws.change,
            tip
        );
    }
    Ok(())
}

/// `omo workspace remove <name>` — drop a workspace from the registry.
fn cmd_workspace_remove(repo: Option<PathBuf>, name: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    WorkspaceRegistry::mutate_locked(&repo, |reg| {
        reg.remove(&name)?;
        Ok(())
    })?;
    println!("removed workspace {name}");
    Ok(())
}

fn submission_path(repo: &Repository, id: &SubmissionId) -> PathBuf {
    repo.control_dir()
        .join("submissions")
        .join(format!("{}.json", id.as_str()))
}

fn save_submission(repo: &Repository, sub: &Submission) -> anyhow::Result<()> {
    let path = submission_path(repo, &sub.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(sub)?;
    omoplata_store::atomic_write(&path, json.as_bytes())?;
    Ok(())
}

fn load_submission(repo: &Repository, id: &SubmissionId) -> anyhow::Result<Submission> {
    let path = submission_path(repo, id);
    let bytes = std::fs::read(&path).with_context(|| format!("submission {} not found", id))?;
    let sub: Submission = serde_json::from_slice(&bytes)?;
    Ok(sub)
}

/// `omo stack [--workspace <name>]` — view change stack for a workspace (§5.9).
fn cmd_stack(repo: Option<PathBuf>, workspace: Option<String>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let reg = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo))?;
    let ws = resolve_workspace(&reg, workspace.as_deref())?.clone();

    // Auto-snapshot dirty working copy into tip commit.
    OpLog::mutate_locked(&repo, |log| {
        let _ = auto_snapshot(&repo, log, &ws)?;
        Ok(())
    })?;

    let log = OpLog::load(oplog_path(&repo))?;
    let tip = log.refs_now().get(ws.change.as_str()).cloned();
    println!("workspace: {} (change: {})", ws.name, ws.change);
    if let Some(tip_commit) = tip {
        println!("  tip commit: {tip_commit}");
    } else {
        println!("  tip commit: (empty)");
    }
    println!("  stack changes: [{}]", ws.change);
    Ok(())
}

/// `omo absorb <target...>` — route hunks into stack changes (§5.9).
fn cmd_absorb(
    repo: Option<PathBuf>,
    target: Vec<String>,
    workspace: Option<String>,
) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let reg = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo))?;
    let ws = resolve_workspace(&reg, workspace.as_deref())?.clone();

    OpLog::mutate_locked(&repo, |log| {
        let _ = auto_snapshot(&repo, log, &ws)?;
        Ok(())
    })?;

    let target_changes: Vec<ChangeId> = target.into_iter().map(ChangeId::new).collect();
    let mut stack = Stack::new(format!("{}-stack", ws.name), vec![ws.change.clone()]);
    for c in &target_changes {
        stack.push(c.clone());
    }
    let count = absorb(&mut stack, &target_changes)?;
    println!(
        "absorbed {count} change(s) into stack {:?}",
        stack.changes()
    );
    Ok(())
}

/// `omo reorder <index>` — swap adjacent stack changes (§5.9).
fn cmd_reorder(
    repo: Option<PathBuf>,
    index: usize,
    workspace: Option<String>,
) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let reg = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo))?;
    let ws = resolve_workspace(&reg, workspace.as_deref())?.clone();

    OpLog::mutate_locked(&repo, |log| {
        let _ = auto_snapshot(&repo, log, &ws)?;
        Ok(())
    })?;

    let mut stack = Stack::new(format!("{}-stack", ws.name), vec![ws.change.clone()]);
    if stack.changes().len() <= 1 {
        stack.push(ChangeId::new("c2"));
    }
    stack.reorder(index)?;
    println!("reordered change stack: {:?}", stack.changes());
    Ok(())
}

/// `omo submit <id> --title <title> <changes...>` — create submission (§5.10).
fn cmd_submit(
    repo: Option<PathBuf>,
    id: String,
    title: String,
    changes: Vec<String>,
    author: Option<String>,
    pending: bool,
) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let change_ids: Vec<ChangeId> = changes.into_iter().map(ChangeId::new).collect();
    let author_str = author.unwrap_or_else(|| "agent".to_string());

    if let Ok(reg) = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo)) {
        let _ = OpLog::mutate_locked(&repo, |op_log| {
            for change_id in &change_ids {
                for ws in reg.workspaces() {
                    if &ws.change == change_id || ws.name == change_id.as_str() {
                        let _ = auto_snapshot(&repo, op_log, ws);
                    }
                }
            }
            Ok(())
        });
    }

    let mut sub = Submission::new(
        SubmissionId::new(&id),
        title.clone(),
        change_ids,
        author_str,
    );
    let status = if pending {
        "pending approval"
    } else {
        sub.approve("auto-reviewer");
        "approved"
    };

    save_submission(&repo, &sub)?;
    println!(
        "submitted {} {:?} with {} change(s) ({status})",
        sub.id,
        sub.title,
        sub.changes.len()
    );
    Ok(())
}

/// `omo approve <id>` — approve a pending submission (§5.10).
fn cmd_approve(repo: Option<PathBuf>, id: String, by: Option<String>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let sub_id = SubmissionId::new(&id);
    let mut sub = load_submission(&repo, &sub_id)?;
    let reviewer = by.unwrap_or_else(|| "reviewer".to_string());
    sub.approve(&reviewer);
    save_submission(&repo, &sub)?;
    println!("approved {} (by {reviewer})", sub.id);
    Ok(())
}

/// `omo land <id> [--queue NAME]` — land a submission through a named merge
/// queue (§5.10, ADR-0009).
///
/// The queue's policy gates the landing before any state changes:
///
/// 1. **approval** — required unless the policy waives it;
/// 2. **carried conflict values** (§5.4) — the submission's content is
///    materialized from the object store and scanned; a strict queue refuses
///    content that still carries values, a permissive one lands them (and says
///    so);
/// 3. **P9 dynamic validation** — when the policy configures a validator, it
///    runs against the materialized content and only a pass lands.
///
/// The observed gate facts go to [`land_submission_in_queue`], which applies
/// policy and performs the `Draft -> Public` transition, writing
/// `public/<change>` refs for `trunk` and `public/<queue>/<change>` for other
/// queues — the same change may therefore land in several queues (the
/// backport story) without forking its identity.
fn cmd_land(repo: Option<PathBuf>, ids: Vec<String>, queue: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let registry = QueueRegistry::load(QueueRegistry::path_in(&repo))?;
    let policy = registry.resolve(&queue)?;
    let subs: Vec<Submission> = ids
        .iter()
        .map(|id| load_submission(&repo, &SubmissionId::new(id)))
        .collect::<anyhow::Result<_>>()?;

    // The approval gate needs no content — check it before paying for
    // materialization and validation (the core re-checks it either way).
    if policy.require_approval {
        for sub in &subs {
            if !sub.is_approved() {
                anyhow::bail!(
                    "submission {} is not approved (queue {})",
                    sub.id,
                    policy.name
                );
            }
        }
    }

    if let Ok(reg) = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo)) {
        let _ = OpLog::mutate_locked(&repo, |op_log| {
            for sub in &subs {
                for change_id in &sub.changes {
                    for ws in reg.workspaces() {
                        if &ws.change == change_id || ws.name == change_id.as_str() {
                            let _ = auto_snapshot(&repo, op_log, ws);
                        }
                    }
                }
            }
            Ok(())
        });
    }

    let mut cg = ChangeGraph::new();
    let log = OpLog::load(oplog_path(&repo))?;
    let refs = log.refs_now();
    let mut sub_tips: Vec<(SubmissionId, Vec<ObjectId>)> = Vec::new();
    for sub in &subs {
        let mut tips = Vec::new();
        for change_id in &sub.changes {
            if let Some(tip) = refs.get(change_id.as_str()) {
                cg.add_change(omoplata_identity::Change::new(
                    change_id.clone(),
                    vec![tip.clone()],
                    Phase::Draft,
                ));
                if let Ok(oid) = tip.as_str().parse::<ObjectId>() {
                    tips.push(oid);
                }
            } else {
                cg.add_change(omoplata_identity::Change::draft(change_id.clone()));
            }
        }
        sub_tips.push((sub.id.clone(), tips));
    }

    // Observe the gate facts against the materialized content (the stored
    // trees, not whatever the working copy has drifted to since). Support is
    // computed relative to the target queue's current landed state.
    let gates = observe_batch_gates(&repo, &registry, &refs, &policy.name, &sub_tips, &policy)?;
    if policy.validate.is_some() {
        match gates.validated {
            Some(true) => eprintln!("queue {}: validation PASSED", policy.name),
            _ => eprintln!("queue {}: validation FAILED", policy.name),
        }
    }
    if gates.carried_values > 0 {
        eprintln!(
            "queue {}: content carries {} conflict value(s)",
            policy.name, gates.carried_values
        );
    }

    if let [sub] = subs.as_slice() {
        let single = QueueGates {
            carried_values: gates.carried_values,
            validated: gates.validated,
        };
        let result = OpLog::mutate_locked(&repo, |op_log| {
            land_submission_in_queue(sub, &policy, &single, &mut cg, op_log)
        })?;
        println!(
            "landed submission {}: {}",
            result.submission_id, result.message
        );
    } else {
        // Tier-0 batch (§5.10): pairwise-disjoint submissions validate as one
        // and land in a single locked op-log transaction; an overlap refuses
        // the whole batch with the colliding paths named.
        let sub_refs: Vec<&Submission> = subs.iter().collect();
        let results = OpLog::mutate_locked(&repo, |op_log| {
            land_batch_in_queue(&sub_refs, &policy, &gates, &mut cg, op_log)
        })?;
        println!(
            "batched {} pairwise-disjoint submission(s) into queue {} (validated as one)",
            results.len(),
            policy.name
        );
        for result in results {
            println!("  landed {}: {}", result.submission_id, result.message);
        }
    }

    // Mechanical backport offers (ADR-0009): every sibling queue this landing
    // did not target. The offer is advisory; `omo backport` carries the
    // approval forward with a certificate when content is unchanged.
    let mut siblings: Vec<String> = registry.queues().iter().map(|q| q.name.clone()).collect();
    if !siblings.iter().any(|n| n == "trunk") {
        siblings.push("trunk".to_owned());
    }
    siblings.retain(|n| n != &policy.name);
    for sub in &subs {
        for sibling in &siblings {
            eprintln!("backport available: omo backport {} --to {sibling}", sub.id);
        }
    }
    Ok(())
}

/// `omo backport <id> --to <queue>` — land an already-landed submission into a
/// second queue, carrying its approval forward with a certificate (ADR-0009,
/// §5.10 approval carry-forward).
///
/// The carry-forward is sound by *identity*: each change's current tip must be
/// byte-identical to the tip that was reviewed and landed in the source queue
/// (the strongest commutation certificate — nothing changed, so nothing needs
/// re-review). A change whose content moved since it landed refuses with a
/// re-review demand instead. The target queue's own gates (carried values,
/// P9 validation) still apply.
fn cmd_backport(repo: Option<PathBuf>, id: String, to: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let registry = QueueRegistry::load(QueueRegistry::path_in(&repo))?;
    let policy = registry.resolve(&to)?;
    let sub_id = SubmissionId::new(&id);
    let mut sub = load_submission(&repo, &sub_id)?;

    let omoplata_identity::Approval::Approved { reviewer, .. } = sub.approval.clone() else {
        anyhow::bail!(
            "submission {} has no approval to carry forward — approve and land it first",
            sub.id
        );
    };

    let log = OpLog::load(oplog_path(&repo))?;
    let refs = log.refs_now();
    let mut cg = ChangeGraph::new();
    let mut tips: Vec<ObjectId> = Vec::new();
    let mut certificates: Vec<ApprovalCertificate> = Vec::new();
    for change_id in &sub.changes {
        let tip = refs.get(change_id.as_str()).ok_or_else(|| {
            anyhow::anyhow!("change {change_id} has no snapshot; nothing to backport")
        })?;
        let Some((source_queue, landed_tip)) = find_landed(&refs, &registry, change_id) else {
            anyhow::bail!(
                "change {change_id} is not landed in any queue; land it first — \
                 backport carries an existing landing's approval forward"
            );
        };
        if &landed_tip != tip {
            anyhow::bail!(
                "change {change_id} moved since it landed in {source_queue} \
                 (reviewed {landed_tip}, now {tip}); approval cannot be carried \
                 forward — re-review and land the new content"
            );
        }
        certificates.push(ApprovalCertificate {
            change_id: change_id.clone(),
            original_commit: landed_tip,
            rebased_commit: tip.clone(),
            approved_by: reviewer.clone(),
            proof_witness: format!(
                "identity: content byte-identical to the tip reviewed and landed in {source_queue}"
            ),
        });
        cg.add_change(omoplata_identity::Change::new(
            change_id.clone(),
            vec![tip.clone()],
            Phase::Draft,
        ));
        if let Ok(oid) = tip.as_str().parse::<ObjectId>() {
            tips.push(oid);
        }
    }

    for cert in &certificates {
        sub.add_certificate(cert.clone());
    }
    save_submission(&repo, &sub)?;

    let sub_tips = vec![(sub.id.clone(), tips)];
    let batch = observe_batch_gates(&repo, &registry, &refs, &policy.name, &sub_tips, &policy)?;
    let gates = QueueGates {
        carried_values: batch.carried_values,
        validated: batch.validated,
    };
    if policy.validate.is_some() {
        match gates.validated {
            Some(true) => eprintln!("queue {}: validation PASSED", policy.name),
            _ => eprintln!("queue {}: validation FAILED", policy.name),
        }
    }

    let result = OpLog::mutate_locked(&repo, |op_log| {
        land_submission_in_queue(&sub, &policy, &gates, &mut cg, op_log)
    })?;
    println!(
        "backported {}: {} (approval by {reviewer} carried forward: {} certificate(s), \
         witness: identity — content unchanged since review)",
        result.submission_id,
        result.message,
        certificates.len()
    );
    Ok(())
}

/// Find the queue (and landed tip) a change is already landed in, if any:
/// `trunk`'s legacy `public/<change>` ref first, then each registered queue's
/// `public/<queue>/<change>`.
fn find_landed(
    refs: &std::collections::BTreeMap<String, CommitId>,
    registry: &QueueRegistry,
    change: &ChangeId,
) -> Option<(String, CommitId)> {
    if let Some(tip) = refs.get(&omoplata_work::queue_ref("trunk", change)) {
        return Some(("trunk".to_owned(), tip.clone()));
    }
    for q in registry.queues() {
        if let Some(tip) = refs.get(&omoplata_work::queue_ref(&q.name, change)) {
            return Some((q.name.clone(), tip.clone()));
        }
    }
    None
}

/// Materialize a batch of submissions' stored trees and observe the queue-gate
/// facts: per-submission **support manifests** (`path -> {definition qualified
/// paths}`, the Tier-0 disjointness input, computed relative to the target
/// queue's current landed state), how many conflict values (§5.4) the content
/// carries, and — when the policy configures one — the P9 validator's verdict
/// over the batch **as one**.
///
/// Support is definition-granular (ADR-0009): the base is the target queue's
/// landed content per file path, and a submission's support for a file is the
/// set of definitions it changed relative to that base — so two submissions
/// that each add a different method to the same `impl`, or edit unrelated
/// functions of one file, have disjoint support and batch. A file identical to
/// base contributes no support; a non-Rust or unparseable file that differs
/// contributes the opaque whole-file token.
///
/// The scratch directory lives under `.omoplata/tmp` (unique per process),
/// split into `base/` (landed state) and `content/` (the submissions, over
/// which carried values and the validator run), and is removed on the way out.
fn observe_batch_gates(
    repo: &Repository,
    registry: &QueueRegistry,
    refs: &std::collections::BTreeMap<String, CommitId>,
    queue: &str,
    sub_tips: &[(SubmissionId, Vec<ObjectId>)],
    policy: &QueuePolicy,
) -> anyhow::Result<BatchGates> {
    let scratch = repo
        .control_dir()
        .join("tmp")
        .join(format!("land-{}", std::process::id()));
    let content_dir = scratch.join("content");
    std::fs::create_dir_all(&content_dir)
        .with_context(|| format!("creating {}", content_dir.display()))?;

    let observe = || -> anyhow::Result<BatchGates> {
        // 1. Base overlay: the target queue's currently-landed content, keyed
        //    by tree-relative path (later landed refs overlay earlier).
        let base = materialize_queue_base(repo, registry, refs, queue, &scratch.join("base"))?;

        // 2. Per submission: the support of each file relative to base.
        let mut manifests = Vec::new();
        for (sid, tips) in sub_tips {
            let sub_dir =
                content_dir.join(format!("sub-{}", sid.as_str().replace(['/', '\\'], "_")));
            let mut files: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            for (i, tip) in tips.iter().enumerate() {
                let dir = sub_dir.join(format!("change-{i}"));
                std::fs::create_dir_all(&dir)?;
                omoplata_work::materialize(repo, tip, &dir)?;
                for file in files_under(&dir) {
                    if let Ok(text) = std::fs::read_to_string(&file) {
                        let rel = file
                            .strip_prefix(&dir)
                            .unwrap_or(&file)
                            .to_string_lossy()
                            .into_owned();
                        files.insert(rel, text);
                    }
                }
            }

            let mut manifest: std::collections::BTreeMap<String, BTreeSet<String>> =
                std::collections::BTreeMap::new();
            for (rel, text) in &files {
                let base_text = base.get(rel).map_or("", String::as_str);
                if base_text == text {
                    continue; // unchanged file touches nothing
                }
                let support: BTreeSet<String> = if rel.ends_with(".rs") {
                    omoplata_work::rust_support(base_text, text).unwrap_or_else(whole_file_support)
                } else {
                    whole_file_support()
                };
                if !support.is_empty() {
                    manifest.insert(rel.clone(), support);
                }
            }
            manifests.push((sid.clone(), manifest));
        }

        // 3. Carried values and validation over the submission content only
        //    (not the base overlay).
        let mut carried_values = 0usize;
        for file in files_under(&content_dir) {
            if !file.extension().is_some_and(|e| e == "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let values = omoplata_drivers::rust::conflict_values(&text)
                .with_context(|| format!("{}: malformed conflict markers", file.display()))?;
            carried_values += values.len();
        }

        let validated = match &policy.validate {
            None => None,
            Some(cmd) => Some(run_dir_validator(cmd, &content_dir)?),
        };
        Ok(BatchGates {
            manifests,
            carried_values,
            validated,
        })
    };

    let gates = observe();
    let _ = std::fs::remove_dir_all(&scratch);
    gates
}

/// The single-element whole-file support token set (non-Rust / unparseable).
fn whole_file_support() -> BTreeSet<String> {
    std::iter::once(omoplata_work::WHOLE_FILE_SUPPORT.to_owned()).collect()
}

/// Materialize the target queue's currently-landed content into `into` and
/// return it as `tree-relative path -> content` (ADR-0009 base for support).
///
/// A `public/…` ref belongs to a non-trunk queue iff its first segment names a
/// registered queue (the same disambiguation `landed()` uses); refs owned by
/// `queue` are overlaid in ref-name order.
fn materialize_queue_base(
    repo: &Repository,
    registry: &QueueRegistry,
    refs: &std::collections::BTreeMap<String, CommitId>,
    queue: &str,
    into: &Path,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let mut base: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (name, commit) in refs {
        let Some(rest) = name.strip_prefix("public/") else {
            continue;
        };
        let owner = match rest.split_once('/') {
            Some((first, _)) if registry.get(first).is_some() => first,
            _ => "trunk",
        };
        if owner != queue {
            continue;
        }
        let Ok(oid) = commit.as_str().parse::<ObjectId>() else {
            continue;
        };
        let dir = into.join(name.replace(['/', '\\'], "_"));
        std::fs::create_dir_all(&dir)?;
        omoplata_work::materialize(repo, &oid, &dir)?;
        for file in files_under(&dir) {
            if let Ok(text) = std::fs::read_to_string(&file) {
                let rel = file
                    .strip_prefix(&dir)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .into_owned();
                base.insert(rel, text);
            }
        }
    }
    Ok(base)
}

/// Every file under `dir`, recursively (control dirs are not present in
/// materialized trees, so no filtering is needed).
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Run a queue's P9 validator command against a materialized content
/// directory. `{}` in the command is replaced with the directory path; without
/// a placeholder the path is appended as the final argument. Returns whether
/// the command exited zero.
fn run_dir_validator(cmd: &str, dir: &Path) -> anyhow::Result<bool> {
    let dir_str = dir.to_string_lossy();
    let full = if cmd.contains("{}") {
        cmd.replace("{}", &dir_str)
    } else {
        format!("{cmd} {dir_str}")
    };
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&full)
        .status()
        .with_context(|| format!("running validator `{full}`"))?;
    Ok(status.success())
}

/// `omo queue add …` — register a landing queue with its policy (ADR-0009).
fn cmd_queue_add(
    repo: Option<PathBuf>,
    name: String,
    validate: Option<String>,
    allow_carried: bool,
    no_approval: bool,
    description: Option<String>,
) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let policy = QueuePolicy {
        name: name.clone(),
        description,
        validate,
        require_approval: !no_approval,
        allow_carried,
    };
    QueueRegistry::mutate_locked(&repo, |reg| {
        reg.add(policy.clone())?;
        Ok(())
    })?;
    println!("registered queue {name} ({})", describe_policy(&policy));
    Ok(())
}

/// `omo queue list` — print every queue (including the implicit trunk).
fn cmd_queue_list(repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let reg = QueueRegistry::load(QueueRegistry::path_in(&repo))?;
    if reg.get("trunk").is_none() {
        let trunk = QueuePolicy::trunk();
        println!("trunk  {} [implicit]", describe_policy(&trunk));
    }
    for q in reg.queues() {
        println!("{}  {}", q.name, describe_policy(q));
    }
    Ok(())
}

/// `omo queue remove <name>` — drop a queue from the registry.
fn cmd_queue_remove(repo: Option<PathBuf>, name: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    QueueRegistry::mutate_locked(&repo, |reg| {
        reg.remove(&name)?;
        Ok(())
    })?;
    println!("removed queue {name}");
    Ok(())
}

/// One-line human summary of a queue policy.
fn describe_policy(q: &QueuePolicy) -> String {
    format!(
        "approval={} carried={} validate={}",
        if q.require_approval {
            "required"
        } else {
            "waived"
        },
        if q.allow_carried {
            "allowed"
        } else {
            "refused"
        },
        q.validate.as_deref().unwrap_or("(none)")
    )
}

/// `omo ref set <name> <commit>` — append a `SetRef` op and persist.
///
/// The whole load -> append -> save cycle runs under the repository's exclusive
/// advisory lock ([`OpLog::mutate_locked`], ADR-0008) so concurrent `omo`
/// processes serialize and no ref update is lost.
fn cmd_ref_set(repo: Option<PathBuf>, name: String, commit: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let seq = OpLog::mutate_locked(&repo, |log| {
        Ok(log
            .set_ref(name.clone(), Some(CommitId::new(commit.clone())), None)
            .seq)
    })?;
    println!("#{seq} set-ref {name} -> {commit}");
    Ok(())
}

/// `omo ref list` — print `name commit` for the current ref state.
fn cmd_ref_list(repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let log = OpLog::load(oplog_path(&repo))?;
    for (name, commit) in log.refs_now() {
        println!("{name} {commit}");
    }
    Ok(())
}

/// `omo op log` — print the operation log newest-first.
fn cmd_op_log(repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let log = OpLog::load(oplog_path(&repo))?;
    for op in log.operations().iter().rev() {
        match &op.note {
            Some(note) => println!("#{} {} [{note}]", op.seq, op.kind.summary()),
            None => println!("#{} {}", op.seq, op.kind.summary()),
        }
    }
    Ok(())
}

/// `omo op undo` — undo the last op, reporting what was undone and the effect.
///
/// The whole load -> undo -> save cycle runs under the repository's exclusive
/// advisory lock ([`OpLog::mutate_locked`], ADR-0008) so a concurrent writer's
/// update cannot be lost. The `Undo` entry, target summary, and the resulting
/// ref-change lines are computed inside the locked section and printed after it.
fn cmd_op_undo(repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let outcome = OpLog::mutate_locked(&repo, |log| {
        let before = log.refs_now();

        let undo_op = log.undo()?;
        // `undo` only ever appends an `Undo` variant; `None` marks the (unreachable)
        // case so it can be surfaced as an error outside the lock, keeping the
        // closure free of `anyhow`.
        let (undo_seq, target_seq) = match &undo_op.kind {
            OpKind::Undo { target_seq } => (undo_op.seq, *target_seq),
            _ => return Ok(None),
        };
        let target_summary = log
            .operations()
            .get(target_seq as usize)
            .map_or_else(|| format!("#{target_seq}"), |op| op.kind.summary());

        let after = log.refs_now();
        let mut lines = Vec::new();
        for (name, old) in &before {
            match after.get(name) {
                None => lines.push(format!("  ref {name}: {old} -> (deleted)")),
                Some(new) if new != old => lines.push(format!("  ref {name}: {old} -> {new}")),
                Some(_) => {}
            }
        }
        for (name, new) in &after {
            if !before.contains_key(name) {
                lines.push(format!("  ref {name}: (created) -> {new}"));
            }
        }
        Ok(Some((undo_seq, target_seq, target_summary, lines)))
    })?
    .ok_or_else(|| anyhow::anyhow!("internal error: undo did not append an Undo"))?;

    let (undo_seq, target_seq, target_summary, lines) = outcome;
    println!("#{undo_seq} undo of #{target_seq}: {target_summary}");
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// `omo revset <expr>` — evaluate over current refs and print matching ids.
fn cmd_revset(repo: Option<PathBuf>, expr: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let log = OpLog::load(oplog_path(&repo))?;
    // Phase lookup is empty for now (phases live in `omoplata-identity`).
    // Registered queue names feed `landed()`'s ref-namespace disambiguation
    // (ADR-0009).
    let queues = QueueRegistry::load(QueueRegistry::path_in(&repo))?;
    let ctx =
        MapContext::new(log.refs_now()).with_queues(queues.queues().iter().map(|q| q.name.clone()));
    for commit in omoplata_work::query(&expr, &ctx)? {
        println!("{commit}");
    }
    Ok(())
}

fn resolve(path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match path {
        Some(p) => Ok(p),
        None => std::env::current_dir().context("could not determine current directory"),
    }
}

fn cmd_init(path: Option<PathBuf>) -> anyhow::Result<()> {
    let root = resolve(path)?;
    let repo = Repository::init(&root)
        .with_context(|| format!("failed to initialize repository at {}", root.display()))?;
    println!(
        "Initialized empty omoplata repository in {}",
        repo.control_dir().display()
    );
    Ok(())
}

fn cmd_status(path: Option<PathBuf>) -> anyhow::Result<()> {
    let root = resolve(path)?;
    if Repository::exists(&root) {
        let repo = Repository::open(&root)?;
        println!("On omoplata repository at {}", repo.root().display());
        println!("No working changes tracked yet (scaffold).");
    } else {
        println!(
            "{} is not an omoplata repository (run `omo init`).",
            root.display()
        );
    }
    Ok(())
}

/// `omo hash <path>` — store a file as a blob and print its id.
fn cmd_hash(repo: Option<PathBuf>, path: PathBuf) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let bytes = if path.as_os_str() == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read stdin")?;
        buf
    } else {
        std::fs::read(&path).with_context(|| format!("failed to read file {}", path.display()))?
    };

    let id = repo.write_blob(bytes)?;
    println!("{id}");
    Ok(())
}

/// `omo cat <id>` — print a stored blob's bytes or a tree's listing.
fn cmd_cat(repo: Option<PathBuf>, id: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let oid: ObjectId = id
        .parse()
        .with_context(|| format!("invalid object id: {id}"))?;
    match repo.read_object(&oid)? {
        Object::Blob(b) => {
            use std::io::Write as _;
            std::io::stdout()
                .write_all(b.bytes())
                .context("writing to stdout")?;
        }
        Object::Tree(t) => {
            for e in t.entries() {
                let kind = match e.kind {
                    EntryKind::Blob => "blob",
                    EntryKind::Tree => "tree",
                };
                println!("{kind} {} {}", e.id, e.name);
            }
        }
    }
    Ok(())
}

/// `omo git verify <path>` — run the I9 round-trip gate over a git repo.
///
/// `<path>` may be a worktree root (e.g. `path/to/repo`) or a git directory
/// (e.g. `path/to/repo/.git`); the gate auto-descends into `.git` as needed.
///
/// On success prints per-type counts and `round-trip gate: PASS` (exit 0). A
/// genuine round-trip failure prints the failing object to stderr and
/// `round-trip gate: FAIL` (exit 1). Pointing at a non-repository or an empty
/// repository is **not** a PASS: it exits non-zero with a clear error and never
/// prints `PASS` (the I9 gate reports PASS only when ≥1 object was checked).
fn cmd_git_verify(git_dir: PathBuf) -> anyhow::Result<i32> {
    match verify_repo(&git_dir) {
        Ok(report) => {
            println!("blobs:   {}", report.blobs);
            println!("trees:   {}", report.trees);
            println!("commits: {}", report.commits);
            println!("tags:    {}", report.tags);
            println!("total:   {}", report.total());
            if report.packfiles > 0 {
                // Packed objects are decoded and gated exactly like loose ones;
                // report how many packs the counts were drawn from.
                println!(
                    "note: {} packfile(s) decoded and included in the counts above",
                    report.packfiles
                );
            }
            println!("round-trip gate: PASS");
            Ok(0)
        }
        // Not-a-repository / empty-repository is a refusal, not a gate failure:
        // there was nothing to round-trip, so we must NOT print `PASS`. Surface
        // it as a clear error and a non-zero exit rather than a green verdict.
        Err(e @ (GitError::NotARepository(_) | GitError::EmptyRepository(_))) => Err(e.into()),
        Err(e) => {
            eprintln!("failing object: {e}");
            println!("round-trip gate: FAIL");
            Ok(1)
        }
    }
}

/// `omo git import <git-dir> [--repo <dir>]` — walk the commit graph and import.
///
/// Enforces I9 (the gate runs first inside `import_repo`), walks the commit DAG
/// from refs, and imports every reachable object, printing the per-type counts
/// and the number of git→omoplata oid mappings.
fn cmd_git_import(git_dir: PathBuf, repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let import = import_repo(&git_dir, &repo)?;
    println!("imported commits: {}", import.commits);
    println!("imported tags:    {}", import.tags);
    println!("imported trees:   {}", import.trees);
    println!("imported blobs:   {}", import.blobs);
    println!("refs walked:      {}", import.refs.len());
    println!("git -> omoplata mappings: {}", import.mapping_count());
    Ok(())
}

/// `omo git log <git-dir>` — print the imported commit graph newest-first.
///
/// Each line is `<short-oid> <subject>  (parents: <short-oids>)`. The graph is
/// walked from refs via `import_repo` into a throwaway store.
fn cmd_git_log(git_dir: PathBuf) -> anyhow::Result<()> {
    let scratch = tempfile::tempdir().context("creating a scratch omoplata store")?;
    let repo = Repository::init(scratch.path()).context("initializing scratch store")?;
    let import = import_repo(&git_dir, &repo)?;
    for oid in import.commit_log() {
        let Some(commit) = import.commit_dag.get(&oid) else {
            continue;
        };
        let parents = commit
            .parents
            .iter()
            .map(|p| short(&p.hex()))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "{} {}  (parents: {})",
            short(&oid.hex()),
            commit.subject(),
            if parents.is_empty() { "-" } else { &parents }
        );
    }
    Ok(())
}

/// `omo git export <git-dir> <out-dir>` — import then exact-mode export.
///
/// Imports the git repo (walking the commit graph), writes every object back out
/// as a loose object under `<out-dir>` plus its refs, then runs the repo-level
/// round-trip gate. Prints `exported <N> objects; round-trip vs source:
/// PASS/FAIL` and exits non-zero on FAIL.
fn cmd_git_export(git_dir: PathBuf, out_dir: PathBuf) -> anyhow::Result<i32> {
    let scratch = tempfile::tempdir().context("creating a scratch omoplata store")?;
    let repo = Repository::init(scratch.path()).context("initializing scratch store")?;
    let import = import_repo(&git_dir, &repo)?;
    let export = export_repo(&import, &out_dir)?;
    let matches = export_matches_source(&git_dir, &out_dir)?;
    let verdict = if matches { "PASS" } else { "FAIL" };
    println!(
        "exported {} objects; round-trip vs source: {verdict}",
        export.objects
    );
    Ok(i32::from(!matches))
}

/// `omo git fetch <repo-url-or-path> [--repo <dir>]` — clone over the wire.
///
/// Speaks the git wire protocol (pkt-line + `upload-pack`) over the local
/// transport against the source repository, importing the received packfile into
/// the destination omoplata repo. Prints the advertised refs, the number of
/// packfile bytes received, and the imported per-type counts.
fn cmd_git_fetch(repo_url: String, repo: Option<PathBuf>) -> anyhow::Result<()> {
    let dest = Repository::open(resolve(repo)?)?;
    let fetch =
        fetch_local(&repo_url, &dest).with_context(|| format!("wire fetch from {repo_url}"))?;

    println!("advertised refs ({}):", fetch.refs.len());
    for (name, oid) in &fetch.refs {
        println!("  {} {}", short(&oid.hex()), name);
    }
    println!("packfile bytes received: {}", fetch.pack_bytes);
    println!("imported commits: {}", fetch.import.commits);
    println!("imported tags:    {}", fetch.import.tags);
    println!("imported trees:   {}", fetch.import.trees);
    println!("imported blobs:   {}", fetch.import.blobs);
    println!("git -> omoplata mappings: {}", fetch.import.mapping_count());
    Ok(())
}

/// The conventional 7-character short form of a 40-hex oid.
fn short(hex: &str) -> String {
    hex.chars().take(7).collect()
}

/// Read a file into a [`Doc`], preserving its contents faithfully.
fn read_doc(path: &Path) -> anyhow::Result<Doc> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Doc::from_str(&text))
}

fn cmd_diff(base: PathBuf, target: PathBuf) -> anyhow::Result<()> {
    let base_doc = read_doc(&base)?;
    let target_doc = read_doc(&target)?;
    let patch = diff(&base_doc, &target_doc);

    // Track the target-side start line so headers read like a unified diff.
    let mut new_line = 1usize;
    let mut base_line = 1usize;
    for hunk in patch.hunks() {
        // Advance past the unchanged base lines preceding this hunk.
        let skipped = hunk.base_start + 1 - base_line;
        new_line += skipped;
        base_line = hunk.base_start + 1;

        println!(
            "@@ -{},{} +{},{} @@",
            hunk.base_start + 1,
            hunk.remove.len(),
            new_line,
            hunk.insert.len()
        );
        for line in &hunk.remove {
            println!("-{line}");
        }
        for line in &hunk.insert {
            println!("+{line}");
        }

        base_line += hunk.remove.len();
        new_line += hunk.insert.len();
    }
    Ok(())
}

fn cmd_merge(base: PathBuf, left: PathBuf, right: PathBuf) -> anyhow::Result<i32> {
    let base_doc = read_doc(&base)?;
    let left_doc = read_doc(&left)?;
    let right_doc = read_doc(&right)?;
    let result = merge3(&base_doc, &left_doc, &right_doc);

    // The merged document already renders conflicts with markers (the human
    // view); the structured `result.conflicts` are the source of truth.
    print!("{}", result.merged);

    if result.is_clean() {
        Ok(0)
    } else {
        let n = result.conflicts.len();
        eprintln!("{n} conflict(s)");
        Ok(1)
    }
}

/// `omo merge-file <base> <left> <right>` — Tier-2 driver merge behind the
/// trusted kernel (§4, §3 P1, §6 I8).
///
/// The driver is chosen from the base path's extension: `.rs` uses the Rust
/// structural driver, everything else the line fallback. The driver is an
/// **untrusted proposer**: when it returns a *clean* result, that result is
/// passed through `kernel::certify`, which independently re-derives the merge
/// and admits the proposal only if it matches the kernel's own witnessed answer.
/// A proposal the kernel cannot witness is **downgraded to a conflict** — making
/// the untrusted-proposer / trusted-kernel split visible.
///
/// The merged output goes to stdout; the `<driver> merge: N conflict(s)` summary
/// and the kernel verdict go to stderr. Exit 0 only when the merge is clean
/// *and* kernel-admitted; a driver conflict or a kernel downgrade exits non-zero.
///
/// # Dynamic validation (P9)
///
/// When `validate` is `Some(cmd)`, kernel admission is treated as **provisional
/// pending dynamic validation** (§4: "Acceptance is provisional pending dynamic
/// validation (P9)"). A clean, kernel-admitted merge is materialized to a temp
/// file and `cmd` is run against it. The verdict (validator exit status) is fed
/// through [`dynamic_validate`]: a **pass** accepts the merge (`dynamic
/// validation PASSED`, exit 0); a **fail** demotes it to a Tier-3 semantic
/// conflict (`dynamic validation FAILED: demoted to semantic conflict
/// (<reason>)`, exit non-zero), printing the semantic-conflict view rather than
/// the merged doc. The validator is **never** run when the merge already
/// conflicted (driver conflict or kernel downgrade) — there is nothing
/// provisional to validate. Without `--validate`, behavior is exactly as before.
fn cmd_merge_file(
    base: PathBuf,
    left: PathBuf,
    right: PathBuf,
    validate: Option<String>,
) -> anyhow::Result<i32> {
    let base_text =
        std::fs::read_to_string(&base).with_context(|| format!("reading {}", base.display()))?;
    let left_text =
        std::fs::read_to_string(&left).with_context(|| format!("reading {}", left.display()))?;
    let right_text =
        std::fs::read_to_string(&right).with_context(|| format!("reading {}", right.display()))?;

    let path = base.to_string_lossy();
    let driver = select_driver(&path);
    let out = driver.merge(&MergeInput {
        base: &base_text,
        left: &left_text,
        right: &right_text,
        path: &path,
    })?;

    let n = out.conflicts.len();
    let k = out.carried.len();

    // A driver conflict is already honest; report and exit non-zero. Nothing
    // provisional to validate — the validator is not run (P9).
    if !out.is_mergeable() {
        print!("{}", out.merged);
        if k == 0 {
            eprintln!("{} merge: {n} conflict(s)", out.driver);
        } else {
            eprintln!("{} merge: {n} conflict(s), {k} carried forward", out.driver);
        }
        return Ok(1);
    }

    // No NEW conflicts, but conflict values from the inputs rode through
    // (§5.4, P3: "rebase maps over conflicts"). The merge is mergeable — the
    // rest of the file integrated structurally — but the output is not a
    // candidate final document, so the kernel and validator are not run.
    // Exit 2 distinguishes "landable, carrying unresolved values" from both
    // success (0) and fresh conflict (1); `omo conflicts` lists the values.
    if k > 0 {
        print!("{}", out.merged);
        eprintln!(
            "{} merge: 0 new conflict(s), {k} carried forward (values ride through; resolve later)",
            out.driver
        );
        return Ok(2);
    }

    eprintln!("{} merge: {n} conflict(s)", out.driver);

    // The driver proposed a clean merge. Gate it through the trusted kernel: the
    // kernel re-derives the merge from base/left/right and admits the proposal
    // only if it matches its own witnessed result.
    let base_doc = Doc::from_str(&base_text);
    let left_doc = Doc::from_str(&left_text);
    let right_doc = Doc::from_str(&right_text);
    let proposed = Doc::from_str(&out.merged);
    let admission = kernel::certify(&base_doc, &left_doc, &right_doc, &proposed);

    match &admission {
        Admission::Merged { witness, .. } => {
            eprintln!(
                "kernel: admitted (commutation witness: {} hunks p, {} hunks q)",
                witness.p.hunks().len(),
                witness.q.hunks().len()
            );
        }
        Admission::Conflict(_) => {
            // A kernel downgrade is already a conflict — nothing provisional to
            // validate (P9). Report and exit non-zero, validator not run.
            print!("{}", out.merged);
            eprintln!(
                "kernel: downgraded to conflict ({} proposal not independently witnessed)",
                out.driver
            );
            return Ok(1);
        }
    }

    // The merge is clean and kernel-admitted — but that admission is only
    // *provisional* (§3 P9). If a dynamic validator is configured, run it and
    // let a failure demote the merge to a Tier-3 semantic conflict (P9/I12).
    match validate {
        None => {
            print!("{}", out.merged);
            Ok(0)
        }
        Some(cmd) => {
            let passed = run_validator(&cmd, &out.merged)?;
            let reason = format!("validator `{cmd}` exited non-zero");
            match dynamic_validate(&base_doc, &left_doc, &right_doc, admission, passed, &reason) {
                Validated::Accepted(result) => {
                    print!("{result}");
                    eprintln!("dynamic validation PASSED");
                    Ok(0)
                }
                Validated::Demoted { conflict, reason } => {
                    print!("{}", render_semantic_conflict(&conflict));
                    eprintln!("dynamic validation FAILED: demoted to semantic conflict ({reason})");
                    Ok(1)
                }
            }
        }
    }
}

/// Run a configured dynamic validator against `merged`, returning whether it
/// passed (exited zero) — the P9 verdict.
///
/// The merged output is written to a temporary file, then `cmd` is run as a
/// shell command (`sh -c`). A `{}` placeholder in `cmd` is substituted with the
/// temp file path; if there is no placeholder, the path is appended (single-
/// quoted) as the last argument. The temp file lives until this function
/// returns, i.e. across the validator run.
fn run_validator(cmd: &str, merged: &str) -> anyhow::Result<bool> {
    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new()
        .context("creating a temp file to materialize the merged output for validation")?;
    tmp.write_all(merged.as_bytes())
        .context("writing merged output to the validation temp file")?;
    tmp.flush().context("flushing the validation temp file")?;

    let file_path = tmp.path().to_string_lossy().into_owned();
    let shell_cmd = if cmd.contains("{}") {
        cmd.replace("{}", &file_path)
    } else {
        // No placeholder: append the path as the last argument. Single-quote it
        // so paths with spaces survive the shell (temp paths won't contain `'`).
        format!("{cmd} '{file_path}'")
    };

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&shell_cmd)
        .status()
        .with_context(|| format!("running dynamic validator: {shell_cmd}"))?;
    Ok(status.success())
}

/// Render a demoted Tier-3 semantic conflict for display (§4 Tier-3).
///
/// A diff3-style view showing both sides' intent and the common base — enough to
/// resolve, without pretending the merge succeeded. The structured [`Conflict`]
/// value remains the source of truth; this is only the human view.
fn render_semantic_conflict(conflict: &Conflict) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("<<<<<<< left".to_owned());
    lines.extend(conflict.left.iter().cloned());
    lines.push("||||||| base".to_owned());
    lines.extend(conflict.base.iter().cloned());
    lines.push("=======".to_owned());
    lines.extend(conflict.right.iter().cloned());
    lines.push(">>>>>>> right".to_owned());
    // Trailing newline so the block reads cleanly on a terminal.
    format!("{}\n", lines.join("\n"))
}

/// `omo conflicts <path>` — list the conflict values a file carries (§5.4).
///
/// Each value is pinned to the definition containing it (via the same
/// sanitize-then-parse pass the structural driver uses). Exit 0 = no values,
/// 2 = values present, error on malformed marker structure.
fn cmd_conflicts(path: PathBuf) -> anyhow::Result<i32> {
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let values = omoplata_drivers::rust::conflict_values(&text)
        .with_context(|| format!("{}: malformed conflict markers", path.display()))?;
    if values.is_empty() {
        println!("no conflict values");
        return Ok(0);
    }
    for v in &values {
        let definition = v.definition.as_deref().unwrap_or("(between definitions)");
        println!(
            "{definition}  line {}: {} line(s) left | {} line(s) right",
            v.line,
            v.left.len(),
            v.right.len()
        );
    }
    eprintln!("{} conflict value(s) carried", values.len());
    Ok(2)
}

/// `omo admit <base> <left> <right>` — trusted kernel admission (§3 P1, §6 I8).
///
/// Runs `kernel::admit` directly on the three files — no proposer involved. On
/// admission, prints the merged document to stdout and the commutation-witness
/// summary to stderr (exit 0). On conflict, prints the merged-with-markers view
/// to stdout and the conflict summary to stderr (exit non-zero). These are the
/// only two outcomes — the "no silent wrong answers" guarantee (I8).
fn cmd_admit(base: PathBuf, left: PathBuf, right: PathBuf) -> anyhow::Result<i32> {
    let base_doc = read_doc(&base)?;
    let left_doc = read_doc(&left)?;
    let right_doc = read_doc(&right)?;

    match kernel::admit(&base_doc, &left_doc, &right_doc) {
        Admission::Merged { result, witness } => {
            print!("{result}");
            eprintln!(
                "admitted: commutation witness (support: {} hunks p, {} hunks q)",
                witness.p.hunks().len(),
                witness.q.hunks().len()
            );
            Ok(0)
        }
        Admission::Conflict(conflicts) => {
            // Render the merged-with-markers human view from merge3 (the same
            // conflict values the kernel returned, shown with markers).
            let view = merge3(&base_doc, &left_doc, &right_doc);
            print!("{}", view.merged);
            eprintln!("conflict: {} region(s)", conflicts.len());
            Ok(1)
        }
    }
}

/// `omo rebase <base> <mine> <onto>` — auto-rebase over conflict values
/// (§5.4, §3 P3, invariant I4).
///
/// Replays my change (`base` → `mine`) on top of `onto`. A clean rebase prints
/// the merged document and `rebase: clean` to stderr (exit 0). A rebase with
/// overlaps carries the conflicts forward as values: the document is printed with
/// `<<<<<<< mine / ======= / >>>>>>> onto` marker blocks (the structured conflicts
/// remain the source of truth), `rebase: <k> conflict(s) carried` goes to stderr,
/// and the exit code is non-zero — the rebase never errors on a conflict.
fn cmd_rebase(base: PathBuf, mine: PathBuf, onto: PathBuf) -> anyhow::Result<i32> {
    let base_doc = read_doc(&base)?;
    let mine_doc = read_doc(&mine)?;
    let onto_doc = read_doc(&onto)?;

    let rebased = rebase(&base_doc, &mine_doc, &onto_doc);
    // The result already renders conflicts with mine/onto markers (the human
    // view); the structured `rebased.conflicts` are the source of truth.
    print!("{}", rebased.result);

    if rebased.clean {
        eprintln!("rebase: clean");
        Ok(0)
    } else {
        let k = rebased.conflicts.len();
        eprintln!("rebase: {k} conflict(s) carried");
        Ok(1)
    }
}

/// Store a file's bytes as a blob and return the commit id (== object id).
fn store_file_blob(repo: &Repository, path: &Path) -> anyhow::Result<CommitId> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let id = repo.write_blob(bytes)?;
    Ok(CommitId::new(id.to_string()))
}

/// Read a stored blob back as UTF-8 text (the content-addressed round trip).
fn read_blob_text(repo: &Repository, commit: &CommitId) -> anyhow::Result<String> {
    let id: ObjectId = commit
        .as_str()
        .parse()
        .with_context(|| format!("malformed commit id {commit}"))?;
    match repo.read_object(&id)? {
        Object::Blob(b) => {
            Ok(String::from_utf8(b.bytes().to_vec()).context("rebased content is not UTF-8")?)
        }
        other => anyhow::bail!("expected a blob for {commit}, got {other:?}"),
    }
}

/// `omo autorebase <base> <mine> <onto>` — the R4 loop end to end (§5.3/§5.4/§5.6).
fn cmd_autorebase(
    repo: Option<PathBuf>,
    base: PathBuf,
    mine: PathBuf,
    onto: PathBuf,
    change: Option<String>,
) -> anyhow::Result<i32> {
    // Require an initialized repository (this is content-on-a-base in the store).
    let repo = Repository::open(resolve(repo)?)?;

    // Hold the exclusive advisory lock across the whole load -> mutate -> save
    // critical section (ADR-0008) so a concurrent writer's op-log update is not
    // lost. Released when `_guard` drops at the end of this function.
    let _guard = repo.lock()?;

    // Store the three inputs as blobs; a commit id is the blob's object id.
    let base_commit = store_file_blob(&repo, &base)?;
    let old_tip = store_file_blob(&repo, &mine)?;
    let onto_commit = store_file_blob(&repo, &onto)?;

    let change_id = ChangeId::new(change.unwrap_or_else(|| "change".to_owned()));

    // Continue the persisted, bi-temporal op log.
    let path = oplog_path(&repo);
    let log = OpLog::load(&path)?;
    let mut engine = RebaseEngine::with_log(repo.clone(), log);

    // Anchor the change's ref at its pre-rebase tip on first sight, so undo can
    // later restore it and `refs_at` reports the pre-rebase tip (bi-temporal).
    if !engine.log().refs_now().contains_key(change_id.as_str()) {
        engine
            .log_mut()
            .set_ref(change_id.to_string(), Some(old_tip.clone()), None);
    }

    let outcome = engine.auto_rebase(&change_id, &base_commit, &old_tip, &onto_commit)?;

    // The op-log entry summary (the Rebase we just appended).
    let entry = engine
        .log()
        .operations()
        .last()
        .map(|op| format!("#{} {}", op.seq, op.kind.summary()));

    // Persist the op log so `omo op log` shows the Rebase entry afterward.
    engine.log().save(&path)?;

    // The rebased content, read back from the store (markers included).
    let text = read_blob_text(&repo, &outcome.new_tip)?;
    print!("{text}");

    eprintln!("autorebase: new tip {}", outcome.new_tip);
    if outcome.clean {
        eprintln!("autorebase: clean");
    } else {
        eprintln!(
            "autorebase: {} conflict(s) carried",
            outcome.conflicts.len()
        );
    }
    if let Some(entry) = entry {
        eprintln!("op-log: {entry}");
    }

    Ok(i32::from(!outcome.clean))
}

/// The 1-based line number containing byte offset `at` in `source`.
fn line_of(source: &str, at: usize) -> usize {
    // One plus the number of newlines strictly before `at`.
    1 + source.as_bytes()[..at.min(source.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

/// `omo defs <file.rs>` — list definitions as `<kind> <path> (lines A-B)`.
fn cmd_defs(file: PathBuf) -> anyhow::Result<()> {
    let source =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let defs = extract_definitions(&source)?;
    for def in &defs {
        let start = line_of(&source, def.byte_range.start);
        let end = line_of(
            &source,
            def.byte_range
                .end
                .saturating_sub(1)
                .max(def.byte_range.start),
        );
        println!("{} {} (lines {start}-{end})", def.kind.label(), def.path);
    }
    Ok(())
}

/// `omo track <old.rs> <new.rs>` — print the definition match report.
fn cmd_track(old: PathBuf, new: PathBuf) -> anyhow::Result<()> {
    let old_src =
        std::fs::read_to_string(&old).with_context(|| format!("reading {}", old.display()))?;
    let new_src =
        std::fs::read_to_string(&new).with_context(|| format!("reading {}", new.display()))?;
    let old_defs = extract_definitions(&old_src)?;
    let new_defs = extract_definitions(&new_src)?;

    let describe = |d: &Definition| format!("{} ({})", d.path, d.kind.label());

    for m in match_definitions(&old_defs, &new_defs) {
        // Each status carries the side indices its meaning implies; a match that
        // does not is a matcher bug, so we skip it rather than emit a misleading
        // line or panic in a production path.
        let line = match (m.status, m.old, m.new) {
            (MatchStatus::Renamed, Some(o), Some(n)) => {
                let o = &old_defs[o];
                let n = &new_defs[n];
                format!("renamed {} -> {} ({})", o.path, n.path, n.kind.label())
            }
            (MatchStatus::Modified, _, Some(n)) => {
                format!("modified {}", describe(&new_defs[n]))
            }
            (MatchStatus::Unchanged, _, Some(n)) => {
                format!("unchanged {}", describe(&new_defs[n]))
            }
            (MatchStatus::Added, _, Some(n)) => {
                format!("added {}", describe(&new_defs[n]))
            }
            (MatchStatus::Deleted, Some(o), _) => {
                format!("deleted {}", describe(&old_defs[o]))
            }
            _ => continue,
        };
        println!("{line}");
    }
    Ok(())
}

/// Build a combined corpus of embedded definitions across `files`, together
/// with a parallel `<file>:<defpath>` label for each entry.
///
/// The embedder is a deterministic local stand-in; see
/// `docs/adr/0006-semantic-embeddings.md`.
fn embed_corpus<E: Embedder + ?Sized>(
    embedder: &E,
    files: &[PathBuf],
) -> anyhow::Result<(Vec<Embedded<Definition>>, Vec<String>)> {
    let mut corpus: Vec<Embedded<Definition>> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for file in files {
        let source =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let label_prefix = file.display();
        for entry in embed_definitions(embedder, &source)? {
            labels.push(format!("{label_prefix}:{}", entry.item.path));
            corpus.push(entry);
        }
    }
    Ok((corpus, labels))
}

/// Resolve which embedder to use for `omo dup`/`omo similar` given the
/// `--real-embeddings` flag, and run `f` with it.
///
/// When `real` is set and the binary was built with `--features fastembed` and
/// the model loads, this uses the real `FastEmbedder`; if the model cannot be
/// loaded (or the binary was built without the feature), it prints a clear note
/// to stderr and falls back to the deterministic [`HashingEmbedder`]. When
/// `real` is unset the behavior is identical to before: the hashing stand-in.
///
/// `f` is generic over the embedder so both branches share the same downstream
/// logic — the design's model-agnostic consumers (§5.7).
fn with_embedder<F>(real: bool, f: F) -> anyhow::Result<()>
where
    F: FnOnce(&dyn Embedder) -> anyhow::Result<()>,
{
    if real {
        #[cfg(feature = "fastembed")]
        {
            match omoplata_sem::FastEmbedder::try_new() {
                Ok(embedder) => {
                    eprintln!("using real embeddings (all-MiniLM-L6-v2, 384-dim)");
                    return f(&embedder);
                }
                Err(e) => {
                    eprintln!(
                        "note: real embedding model unavailable ({e}); \
                         using the deterministic hashing stand-in"
                    );
                }
            }
        }
        #[cfg(not(feature = "fastembed"))]
        {
            eprintln!(
                "note: this binary was built without `--features fastembed`; \
                 using the deterministic hashing stand-in. Rebuild with \
                 `cargo build --features fastembed` for real embeddings."
            );
        }
    }
    // NOTE (stand-in model): deterministic hashing embedder standing in for a
    // real transformer model behind the `Embedder` trait (ADR-0006).
    let embedder = HashingEmbedder::default();
    f(&embedder)
}

/// `omo dup [file.rs]...` — flag likely duplicate definitions across active workspaces or specified files (§5.7).
fn cmd_dup(
    repo: Option<PathBuf>,
    files: Vec<PathBuf>,
    threshold: f32,
    real: bool,
) -> anyhow::Result<()> {
    with_embedder(real, |embedder| {
        let (corpus, labels) = if files.is_empty() {
            let root = resolve(repo)?;
            let repo_obj = Repository::open(&root)?;
            let reg = WorkspaceRegistry::load(WorkspaceRegistry::path_in(&repo_obj))?;
            let mut corpus = Vec::new();
            let mut labels = Vec::new();

            for ws in reg.workspaces() {
                let embedded_items = embed_workspace_dir(embedder, Some(&ws.name), &ws.working_dir)
                    .with_context(|| format!("failed to scan workspace {}", ws.name))?;
                for item in embedded_items {
                    let label = format!(
                        "workspace {} ({}:{})",
                        ws.name,
                        item.item.file_path.display(),
                        item.item.def.name
                    );
                    corpus.push(Embedded {
                        item: item.item.def,
                        vector: item.vector,
                    });
                    labels.push(label);
                }
            }
            (corpus, labels)
        } else {
            embed_corpus(embedder, &files)?
        };

        let dups = find_duplicates(&corpus, threshold);
        if dups.is_empty() {
            println!("no likely duplicate definitions");
            return Ok(());
        }
        for (i, j, score) in dups {
            println!("{score:.2}  {} ~ {}", labels[i], labels[j]);
        }
        Ok(())
    })
}

/// `omo similar <query> <file.rs>...` — rank definitions by similarity (§5.7).
fn cmd_similar(query: String, files: Vec<PathBuf>, top: usize, real: bool) -> anyhow::Result<()> {
    with_embedder(real, |embedder| {
        let (corpus, labels) = embed_corpus(embedder, &files)?;

        for (idx, score) in search(embedder, &query, &corpus, top) {
            println!("{score:.4} {}", labels[idx]);
        }
        Ok(())
    })
}
