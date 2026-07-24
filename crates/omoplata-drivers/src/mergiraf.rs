//! The **Mergiraf** structural-merge driver — a PATH-detected shell-out.
//!
//! [Mergiraf](https://mergiraf.org) is the external, per-language structural
//! merge tool the design doc names as the Tier-2 *fallback driver* for every
//! language other than Rust (§4 architecture diagram: "Interim driver:
//! Mergiraf"; §8 scope: "Mergiraf as the fallback driver for everything else").
//! It parses base/left/right with tree-sitter grammars for 45+ languages and
//! resolves conflicts a plain line merge cannot — reorderings, disjoint edits to
//! neighbouring declarations, formatting churn — falling back to diff3-style
//! conflict markers only where a genuine structural conflict remains.
//!
//! # Why shell out rather than depend on the crate
//!
//! Mergiraf is distributed as a Rust crate, but this driver invokes the
//! `mergiraf` **binary** rather than linking the library, for two reasons:
//!
//! 1. **Licensing.** Mergiraf is **GPL-3.0-only**. Linking it into
//!    `omoplata-drivers` would force the workspace's license posture to the GPL.
//!    A process boundary keeps Mergiraf a separate program we *invoke*, not a
//!    library we *incorporate*, preserving omoplata's own licensing.
//! 2. **API stability.** Mergiraf's library API is explicitly documented as
//!    unstable and not intended for external consumers, whereas its command-line
//!    interface (`mergiraf merge BASE LEFT RIGHT -p PATH -o OUT`) is the stable,
//!    supported contract.
//!
//! # Trust boundary
//!
//! Mergiraf is an **untrusted proposer** like every other Tier-2 driver
//! (design doc §4 principle **P1**, the LCF architecture). Running it in a
//! separate process across a filesystem boundary — rather than in-process — also
//! contains a merge tool that parses attacker-influenceable input from an
//! untrusted proposer: a crash or misbehaviour in the child is an exit status we
//! observe, not memory we share. Its output is a *candidate* the verified kernel
//! still gates; the structured [`Conflict`](omoplata_algebra::Conflict) values
//! this driver returns are the source of truth (§5.4), and the marker-rendered
//! text is a human view derived from them.
//!
//! # Availability
//!
//! Mergiraf is optional. [`mergiraf_available`] performs a cached
//! (`OnceLock`) probe for the binary on `PATH`; when it is absent,
//! [`select_driver`](crate::select_driver) routes non-Rust paths to the built-in
//! [`LineDriver`](crate::LineDriver) instead, so the crate stays buildable and
//! testable with no external tool (ADR-0004's no-hard-dependency guarantee).

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use omoplata_algebra::Conflict;

use crate::{DriverError, DriverOutput, MergeDriver, MergeInput};

/// The stable conflict-marker labels passed to `mergiraf` so its diff3 output is
/// deterministic and matches this crate's parser. `left`/`base`/`right` line up
/// with [`Conflict`]'s fields.
const LABEL_LEFT: &str = "left";
const LABEL_BASE: &str = "base";
const LABEL_RIGHT: &str = "right";

/// The per-merge budget handed to `mergiraf -t`, in **milliseconds** (that is
/// the unit `mergiraf --help` documents for `--timeout`). Mergiraf falls back to
/// git's own algorithm if structural merging exceeds this, so the child still
/// returns promptly rather than hanging the caller.
const MERGE_TIMEOUT_MS: u64 = 10_000;

/// Whether the `mergiraf` binary is available on `PATH`.
///
/// The probe runs `mergiraf --version` once and caches the boolean in a
/// process-lifetime `OnceLock`; subsequent calls are free. A binary that is
/// present but fails to report its version (non-zero exit, or it cannot be
/// spawned) counts as unavailable.
///
/// [`select_driver`](crate::select_driver) consults this to decide whether a
/// non-Rust path can use [`MergirafDriver`] or must fall back to
/// [`LineDriver`](crate::LineDriver).
#[must_use]
pub fn mergiraf_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("mergiraf")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// The Mergiraf structural-merge driver — the Tier-2 fallback for non-Rust
/// paths when the `mergiraf` binary is on `PATH`.
///
/// Selection is by file extension via [`select_driver`](crate::select_driver);
/// a `MergirafDriver` is only chosen when [`mergiraf_available`] is true and the
/// extension is one Mergiraf supports (see [`supports_extension`]). Constructing
/// one directly and calling [`merge`](MergirafDriver::merge) with the binary
/// absent yields a [`DriverError`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MergirafDriver;

impl MergirafDriver {
    /// Create a new Mergiraf driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl MergeDriver for MergirafDriver {
    fn name(&self) -> &'static str {
        "mergiraf"
    }

    /// Structurally merge `base`, `left`, and `right` by shelling out to
    /// `mergiraf`.
    ///
    /// The three sides are written to a private per-call temporary directory,
    /// each named with the real extension derived from `input.path` so Mergiraf
    /// selects the right grammar (its `-p` pathname also carries the real path
    /// for detection). `mergiraf merge <base> <left> <right> -p <path> -o <out>`
    /// is run with stable marker labels and a wall-clock timeout. On a clean
    /// merge (exit 0) the output file becomes `merged` with no conflicts; on a
    /// conflicted merge (exit 1) the diff3 markers in the output are parsed into
    /// structured [`Conflict`] values (the source of truth).
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Tool`] if the binary is absent, cannot be spawned,
    /// times out, exits with any status other than 0 or 1, or its output cannot
    /// be read — failures are surfaced rather than silently degraded so a
    /// misbehaving tool is visible.
    fn merge(&self, input: &MergeInput) -> Result<DriverOutput, DriverError> {
        let dir = tempfile::Builder::new()
            .prefix("omoplata-mergiraf-")
            .tempdir()
            .map_err(|e| DriverError::tool(format!("creating mergiraf tempdir: {e}")))?;

        let ext = extension_of(input.path);
        let base_path = write_side(dir.path(), "base", ext, input.base)?;
        let left_path = write_side(dir.path(), "left", ext, input.left)?;
        let right_path = write_side(dir.path(), "right", ext, input.right)?;
        let out_path = dir.path().join(match ext {
            Some(e) => format!("out.{e}"),
            None => "out".to_owned(),
        });

        let output = Command::new("mergiraf")
            .arg("merge")
            .arg(&base_path)
            .arg(&left_path)
            .arg(&right_path)
            .arg("-p")
            .arg(input.path)
            .arg("-o")
            .arg(&out_path)
            .arg("-s")
            .arg(LABEL_BASE)
            .arg("-x")
            .arg(LABEL_LEFT)
            .arg("-y")
            .arg(LABEL_RIGHT)
            .arg("-t")
            .arg(MERGE_TIMEOUT_MS.to_string())
            .output()
            .map_err(|e| DriverError::tool(format!("spawning mergiraf: {e}")))?;

        // Exit 0 = clean, exit 1 = conflicts remain; anything else is a failure.
        let code = output.status.code();
        match code {
            Some(0) => {
                let merged = read_out(&out_path)?;
                Ok(DriverOutput {
                    merged,
                    conflicts: Vec::new(),
                    carried: Vec::new(),
            driver: self.name(),
                })
            }
            Some(1) => {
                let merged = read_out(&out_path)?;
                let conflicts = parse_diff3_conflicts(&merged);
                Ok(DriverOutput {
                    merged,
                    conflicts,
                    carried: Vec::new(),
            driver: self.name(),
                })
            }
            other => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let shown = match other {
                    Some(c) => format!("exit {c}"),
                    None => "terminated by signal".to_owned(),
                };
                Err(DriverError::tool(format!(
                    "mergiraf failed ({shown}): {}",
                    stderr.trim()
                )))
            }
        }
    }
}

/// Write one side of the merge to `dir/<name>.<ext>` (or `dir/<name>` when the
/// path has no extension) and return its path.
fn write_side(
    dir: &Path,
    name: &str,
    ext: Option<&str>,
    contents: &str,
) -> Result<std::path::PathBuf, DriverError> {
    let file = dir.join(match ext {
        Some(e) => format!("{name}.{e}"),
        None => name.to_owned(),
    });
    std::fs::write(&file, contents)
        .map_err(|e| DriverError::tool(format!("writing {} for mergiraf: {e}", file.display())))?;
    Ok(file)
}

/// Read Mergiraf's output file into a string.
fn read_out(out_path: &Path) -> Result<String, DriverError> {
    std::fs::read_to_string(out_path).map_err(|e| {
        DriverError::tool(format!(
            "reading mergiraf output {}: {e}",
            out_path.display()
        ))
    })
}

/// The final extension of `path` (ASCII), or `None` if it has none. Mirrors the
/// selection logic in [`crate::select_driver`].
fn extension_of(path: &str) -> Option<&str> {
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, e)| e)
        .filter(|e| !e.is_empty())
}

/// The set of file extensions Mergiraf can structurally merge in v0.18.0.
///
/// This mirrors the extensions reported by `mergiraf languages` for the pinned
/// Mergiraf version: each entry is a lower-cased extension (without the dot).
/// Selection is conservative — an extension not listed here routes to the
/// built-in [`LineDriver`](crate::LineDriver) even when the binary is present, so
/// we never hand Mergiraf a path it would reject. Detection is case-insensitive.
///
/// Mergiraf also recognises some grammars by *bare filename* (e.g. `Makefile`,
/// `go.mod`, `CMakeLists.txt`, `BUILD`). Selection here is extension-based, so
/// those route to [`LineDriver`](crate::LineDriver) — a documented, safe
/// conservatism, not a silent wrong answer.
const SUPPORTED_EXTENSIONS: &[&str] = &[
    // Systems / compiled
    "c",
    "h",
    "cc",
    "hh",
    "cpp",
    "hpp",
    "cxx",
    "hxx",
    "c++",
    "h++",
    "mpp",
    "cppm",
    "ixx",
    "tcc",
    "cs",
    "go",
    "rs",
    "java",
    "kt",
    "kts",
    "scala",
    "sbt",
    "dart",
    "sol",
    "sv",
    "svh",
    "f",
    "for",
    "f90", // Scripting
    "py",
    "rb",
    "php",
    "phtml",
    "php3",
    "php4",
    "php5",
    "phps",
    "phpt",
    "lua",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "tsx",
    "sh",
    "bash",
    "r",
    "gleam",
    "ex",
    "exs",
    // Config / data / markup / other grammars
    "json",
    "yaml",
    "yml",
    "toml",
    "xml",
    "xhtml",
    "html",
    "htm",
    "ini",
    "properties",
    "nix",
    "hcl",
    "tf",
    "tfvars",
    "md",
    "scm",
    "dts",
    "bzl",
    "bxl",
    "bazel",
    "star",
    "sky",
    "cmake",
    "mk",
    "ml",
    "mli",
    "hs",
];

/// Whether Mergiraf supports the final extension of `path`.
///
/// Case-insensitive on the extension. A path with no extension is unsupported.
/// This is the second half of [`select_driver`](crate::select_driver)'s guard
/// (the first being [`mergiraf_available`]).
#[must_use]
pub fn supports_extension(path: &str) -> bool {
    match extension_of(path) {
        Some(ext) => {
            let lower = ext.to_ascii_lowercase();
            SUPPORTED_EXTENSIONS.contains(&lower.as_str())
        }
        None => false,
    }
}

/// Parse Mergiraf's diff3-style conflict markers out of `merged` into structured
/// [`Conflict`] values.
///
/// Mergiraf, invoked with our stable labels, renders each surviving conflict as:
///
/// ```text
/// <<<<<<< left
/// …left lines…
/// ||||||| base
/// …base lines…
/// =======
/// …right lines…
/// >>>>>>> right
/// ```
///
/// Each block yields one `Conflict { base, left, right }` whose vectors are the
/// lines (newline-stripped) of the respective sections, in file order. Text
/// outside conflict blocks is ignored — it is the cleanly-merged remainder. A
/// malformed block (e.g. a `<<<<<<<` with no closing `>>>>>>>`) is dropped rather
/// than panicking; the marker text still lives in `merged` for a human to see.
#[must_use]
pub fn parse_diff3_conflicts(merged: &str) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    let mut lines = merged.lines();
    // Walk lines, opening a block on `<<<<<<<`, switching section on `|||||||`
    // and `=======`, closing on `>>>>>>>`.
    'outer: while let Some(line) = lines.next() {
        if !is_marker(line, "<<<<<<<") {
            continue;
        }
        let mut left = Vec::new();
        let mut base = Vec::new();
        let mut right = Vec::new();
        // Section 0 = left (until `|||||||`), 1 = base (until `=======`),
        // 2 = right (until `>>>>>>>`).
        let mut section = 0u8;
        loop {
            let Some(l) = lines.next() else {
                // Unterminated block: discard and stop scanning.
                break 'outer;
            };
            if is_marker(l, "|||||||") {
                if section == 0 {
                    section = 1;
                    continue;
                }
                // Nested/duplicate marker — malformed; drop this block.
                continue 'outer;
            }
            if is_marker(l, "=======") {
                if section <= 1 {
                    section = 2;
                    continue;
                }
                continue 'outer;
            }
            if is_marker(l, ">>>>>>>") {
                conflicts.push(Conflict { base, left, right });
                continue 'outer;
            }
            match section {
                0 => left.push(l.to_owned()),
                1 => base.push(l.to_owned()),
                _ => right.push(l.to_owned()),
            }
        }
    }
    conflicts
}

/// Whether `line` is a conflict marker line beginning with the 7-character
/// `sigil` (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`). Matches the marker
/// optionally followed by a space-separated label.
fn is_marker(line: &str, sigil: &str) -> bool {
    line == sigil || line.strip_prefix(sigil).is_some_and(|r| r.starts_with(' '))
}
