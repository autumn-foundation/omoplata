//! `omo` — the omoplata command-line interface.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use omoplata_algebra::{diff, merge3, Doc};
use omoplata_drivers::{select_driver, MergeInput};
use omoplata_git::{import_repo, verify_repo};
use omoplata_identity::{
    extract_definitions, match_definitions, CommitId, Definition, MatchStatus,
};
use omoplata_sem::{embed_definitions, find_duplicates, search, Embedded, HashingEmbedder};
use omoplata_store::{EntryKind, Object, ObjectId, Repository};
use omoplata_work::{MapContext, OpKind, OpLog};

/// omoplata: a version control system with a verified merge kernel.
#[derive(Debug, Parser)]
#[command(name = "omo", version, about, long_about = None)]
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
    /// Prints the merged output to stdout and `<driver> merge: N conflict(s)` to
    /// stderr; exits 0 if clean, non-zero otherwise.
    MergeFile {
        /// The common base file.
        base: PathBuf,
        /// The left side.
        left: PathBuf,
        /// The right side.
        right: PathBuf,
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
    /// Import a git repository's blobs and trees into an omoplata repository.
    ///
    /// Enforces I9 (runs the gate first, refusing import if it fails), then maps
    /// git blobs/trees into the store. Prints imported counts and the number of
    /// git→omoplata oid mappings.
    Import {
        /// The git directory to import, e.g. `path/to/.git`.
        git_dir: PathBuf,
        /// Destination omoplata repository (defaults to the current directory).
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
        Command::HashObject { path, repo } => cmd_hash_object(repo, path).map(|()| 0),
        Command::CatObject { id, repo } => cmd_cat_object(repo, id).map(|()| 0),
        Command::Diff { base, target } => cmd_diff(base, target).map(|()| 0),
        Command::Merge { base, left, right } => cmd_merge(base, left, right),
        Command::MergeFile { base, left, right } => cmd_merge_file(base, left, right),
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
        },
        Command::Dup { files, threshold } => cmd_dup(files, threshold).map(|()| 0),
        Command::Similar { query, files, top } => cmd_similar(query, files, top).map(|()| 0),
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

/// `omo git import <git-dir> [--repo <dir>]` — import a git repo into the store.
///
/// Enforces I9 (the gate runs first inside `import_repo`) then imports blobs and
/// trees, printing counts and the number of git→omoplata oid mappings.
fn cmd_git_import(git_dir: PathBuf, repo: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = Repository::open(resolve(repo)?)?;
    let import = import_repo(&git_dir, &repo)?;
    println!("imported blobs:   {}", import.blobs);
    println!("imported trees:   {}", import.trees);
    println!("commits seen:     {}", import.commits);
    println!("tags seen:        {}", import.tags);
    println!("git -> omoplata mappings: {}", import.mapping_count());
    Ok(())
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

/// `omo merge-file <base> <left> <right>` — Tier-2 driver merge (§4).
///
/// The driver is chosen from the base path's extension: `.rs` uses the Rust
/// structural driver, everything else the line fallback. The merged output goes
/// to stdout; a `<driver> merge: N conflict(s)` summary goes to stderr. Exit 0
/// if clean, 1 if conflicted.
fn cmd_merge_file(base: PathBuf, left: PathBuf, right: PathBuf) -> anyhow::Result<i32> {
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

    print!("{}", out.merged);

    let n = out.conflicts.len();
    eprintln!("{} merge: {n} conflict(s)", out.driver);
    if out.is_clean() {
        Ok(0)
    } else {
        Ok(1)
    }
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
        let line = match m.status {
            MatchStatus::Renamed => {
                let o = &old_defs[m.old.expect("renamed has old")];
                let n = &new_defs[m.new.expect("renamed has new")];
                format!("renamed {} -> {} ({})", o.path, n.path, n.kind.label())
            }
            MatchStatus::Modified => {
                format!(
                    "modified {}",
                    describe(&new_defs[m.new.expect("modified has new")])
                )
            }
            MatchStatus::Unchanged => {
                format!(
                    "unchanged {}",
                    describe(&new_defs[m.new.expect("unchanged has new")])
                )
            }
            MatchStatus::Added => {
                format!(
                    "added {}",
                    describe(&new_defs[m.new.expect("added has new")])
                )
            }
            MatchStatus::Deleted => {
                format!(
                    "deleted {}",
                    describe(&old_defs[m.old.expect("deleted has old")])
                )
            }
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
fn embed_corpus(
    embedder: &HashingEmbedder,
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

/// `omo dup <file.rs>...` — flag likely duplicate definitions across files (§5.7).
fn cmd_dup(files: Vec<PathBuf>, threshold: f32) -> anyhow::Result<()> {
    // NOTE (stand-in model): deterministic hashing embedder standing in for a
    // real transformer model behind the `Embedder` trait (ADR-0006).
    let embedder = HashingEmbedder::default();
    let (corpus, labels) = embed_corpus(&embedder, &files)?;

    let dups = find_duplicates(&corpus, threshold);
    if dups.is_empty() {
        println!("no likely duplicate definitions");
        return Ok(());
    }
    for (i, j, score) in dups {
        println!("{score:.2}  {} ~ {}", labels[i], labels[j]);
    }
    Ok(())
}

/// `omo similar <query> <file.rs>...` — rank definitions by similarity (§5.7).
fn cmd_similar(query: String, files: Vec<PathBuf>, top: usize) -> anyhow::Result<()> {
    // NOTE (stand-in model): deterministic hashing embedder standing in for a
    // real transformer model behind the `Embedder` trait (ADR-0006).
    let embedder = HashingEmbedder::default();
    let (corpus, labels) = embed_corpus(&embedder, &files)?;

    for (idx, score) in search(&embedder, &query, &corpus, top) {
        println!("{score:.4} {}", labels[idx]);
    }
    Ok(())
}
