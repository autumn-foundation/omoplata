//! `omo` — the omoplata command-line interface.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use omoplata_algebra::{diff, merge3, Doc};
use omoplata_store::{EntryKind, Object, ObjectId, Repository};

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
    }
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
