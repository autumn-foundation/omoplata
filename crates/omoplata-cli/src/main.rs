//! `omo` — the omoplata command-line interface.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use omoplata_algebra::{
    diff, dynamic_validate, kernel, merge3, rebase, Admission, Conflict, Doc, Validated,
};
use omoplata_drivers::{select_driver, MergeInput};
use omoplata_git::{export_matches_source, export_repo, import_repo, verify_repo};
use omoplata_identity::{
    extract_definitions, match_definitions, ChangeId, CommitId, Definition, MatchStatus,
};
use omoplata_sem::{
    embed_definitions, find_duplicates, search, Embedded, Embedder, HashingEmbedder,
};
use omoplata_store::{EntryKind, Object, ObjectId, Repository};
use omoplata_work::{MapContext, OpKind, OpLog, RebaseEngine};

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
    HashObject {
        /// File to read (use `-` for stdin).
        path: PathBuf,
        /// Repository directory (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print a stored object by id (blob bytes, or a tree listing).
    CatObject {
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
        /// The Rust source files to scan (two or more for cross-file duplicates).
        #[arg(required = true)]
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
        /// The git directory to verify, e.g. `path/to/.git`.
        git_dir: PathBuf,
    },
    /// Import a git repository by walking its commit graph from refs.
    ///
    /// Enforces I9 (runs the gate first, refusing import if it fails), walks the
    /// commit DAG from every ref, and imports every reachable object. Prints the
    /// commit/tag/tree/blob counts and the number of git→omoplata oid mappings.
    Import {
        /// The git directory to import, e.g. `path/to/.git`.
        git_dir: PathBuf,
        /// Destination omoplata repository (defaults to the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Print the imported commit graph, newest-first.
    ///
    /// Each line is `<short-oid> <subject>  (parents: <short-oids>)`.
    Log {
        /// The git directory to read, e.g. `path/to/.git`.
        git_dir: PathBuf,
    },
    /// Exact-mode export: import a git repo, then write every object back out.
    ///
    /// Writes loose objects and refs under `<out-dir>` (a git-directory layout:
    /// `<out-dir>/objects/<xx>/…` and `<out-dir>/refs/…`), then runs the
    /// repo-level round-trip gate. Prints `exported <N> objects; round-trip vs
    /// source: PASS/FAIL` and exits non-zero on FAIL.
    Export {
        /// The git directory to import and export, e.g. `path/to/.git`.
        git_dir: PathBuf,
        /// The output directory to write the reconstructed objects and refs to.
        out_dir: PathBuf,
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
        Command::HashObject { path, repo } => cmd_hash_object(repo, path).map(|()| 0),
        Command::CatObject { id, repo } => cmd_cat_object(repo, id).map(|()| 0),
        Command::Diff { base, target } => cmd_diff(base, target).map(|()| 0),
        Command::Merge { base, left, right } => cmd_merge(base, left, right),
        Command::MergeFile {
            base,
            left,
            right,
            validate,
        } => cmd_merge_file(base, left, right, validate),
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
        },
        Command::Dup {
            files,
            threshold,
            real_embeddings,
        } => cmd_dup(files, threshold, real_embeddings).map(|()| 0),
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
    repo.control_dir().join("oplog.jsonl")
}

/// `omo ref set <name> <commit>` — append a `SetRef` op and persist.
fn cmd_ref_set(repo: Option<PathBuf>, name: String, commit: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let path = oplog_path(&repo);
    let mut log = OpLog::load(&path)?;
    let op = log.set_ref(name.clone(), Some(CommitId::new(commit.clone())), None);
    let seq = op.seq;
    log.save(&path)?;
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
fn cmd_op_undo(repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let path = oplog_path(&repo);
    let mut log = OpLog::load(&path)?;
    let before = log.refs_now();

    let undo_op = log.undo()?;
    let (undo_seq, target_seq) = match &undo_op.kind {
        OpKind::Undo { target_seq } => (undo_op.seq, *target_seq),
        // `undo` only ever appends an `Undo` variant.
        _ => {
            return Err(anyhow::anyhow!(
                "internal error: undo did not append an Undo"
            ))
        }
    };
    let target_summary = log
        .operations()
        .get(target_seq as usize)
        .map_or_else(|| format!("#{target_seq}"), |op| op.kind.summary());

    let after = log.refs_now();
    log.save(&path)?;

    println!("#{undo_seq} undo of #{target_seq}: {target_summary}");
    for (name, old) in &before {
        match after.get(name) {
            None => println!("  ref {name}: {old} -> (deleted)"),
            Some(new) if new != old => println!("  ref {name}: {old} -> {new}"),
            Some(_) => {}
        }
    }
    for (name, new) in &after {
        if !before.contains_key(name) {
            println!("  ref {name}: (created) -> {new}");
        }
    }
    Ok(())
}

/// `omo revset <expr>` — evaluate over current refs and print matching ids.
fn cmd_revset(repo: Option<PathBuf>, expr: String) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let log = OpLog::load(oplog_path(&repo))?;
    // Phase lookup is empty for now (phases live in `omoplata-identity`).
    let ctx = MapContext::new(log.refs_now());
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

fn cmd_hash_object(repo: Option<PathBuf>, path: PathBuf) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let bytes = if path == Path::new("-") {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)
            .context("reading standard input")?;
        buf
    } else {
        std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?
    };
    println!("{}", repo.write_blob(bytes)?);
    Ok(())
}

fn cmd_cat_object(repo: Option<PathBuf>, id: String) -> anyhow::Result<()> {
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

/// `omo git verify <git-dir>` — run the I9 round-trip gate over a git repo.
///
/// On success prints per-type counts and `round-trip gate: PASS` (exit 0). On
/// failure prints the failing object to stderr and `round-trip gate: FAIL`
/// (exit 1).
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

    // A driver conflict is already honest; report and exit non-zero. Nothing
    // provisional to validate — the validator is not run (P9).
    if !out.is_clean() {
        print!("{}", out.merged);
        eprintln!("{} merge: {n} conflict(s)", out.driver);
        return Ok(1);
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

/// `omo dup <file.rs>...` — flag likely duplicate definitions across files (§5.7).
fn cmd_dup(files: Vec<PathBuf>, threshold: f32, real: bool) -> anyhow::Result<()> {
    with_embedder(real, |embedder| {
        let (corpus, labels) = embed_corpus(embedder, &files)?;

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
