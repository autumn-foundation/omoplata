//! `omo` — the omoplata command-line interface.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
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
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Init { path } => cmd_init(path),
        Command::Status { path } => cmd_status(path),
        Command::HashObject { path, repo } => cmd_hash_object(repo, path),
        Command::CatObject { id, repo } => cmd_cat_object(repo, id),
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
