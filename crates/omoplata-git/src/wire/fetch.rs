//! A real `upload-pack` **fetch** client over git's *local* transport.
//!
//! ## What "wire protocol" means here (design doc §3 P8)
//! P8 requires omoplata to read and write *"the git object format **and wire
//! protocol**"*. Git speaks the same pkt-line + `upload-pack` conversation over
//! every transport — the only thing that differs is how the two processes are
//! connected. For `http`/`ssh` URLs that is a socket; for `file://` URLs and
//! local paths git uses the **local transport**, which spawns the server-side
//! `git upload-pack` (fetch) or `git receive-pack` (push) and speaks pkt-line
//! over that child process's stdio. This module implements exactly that: it is a
//! genuine wire-protocol client, run against a local `upload-pack` process rather
//! than a socket. Networked transports are not offline-testable, so they are out
//! of scope for this reduction (see the crate ADR); the protocol code here is the
//! same code those transports would drive.
//!
//! ## The exchange (protocol v0)
//! 1. Spawn `git upload-pack <dir>` with piped stdin/stdout.
//! 2. Read the server's **ref advertisement**: the first pkt-line is
//!    `"<oid> <ref>\0<capabilities>"`, subsequent lines are `"<oid> <ref>"`, and
//!    a flush-pkt ends it.
//! 3. Send a `want <oid> <caps>` line for each advertised ref we intend to clone
//!    (capabilities only on the first want line), a flush-pkt to end the want
//!    list, then a `done` line. We deliberately request **`ofs-delta`** but
//!    **not** `side-band`/`side-band-64k`, so the packfile arrives as a raw byte
//!    stream rather than multiplexed pkt-lines.
//! 4. Read the response: a `NAK` pkt-line, then the **raw packfile bytes** to
//!    EOF.
//! 5. Decode the packfile in memory with [`crate::parse_pack_bytes`] and import
//!    every object through the I9 gate ([`crate::import_objects`]).
//!
//! No `unwrap`/`expect`/`panic` appears outside tests.

use std::collections::HashSet;
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use omoplata_store::Repository;

use crate::error::GitError;
use crate::import::{import_objects, GitImport};
use crate::object::{GitObject, GitOid};
use crate::pack::parse_pack_bytes;
use crate::wire::pkt::{
    parse_ref_line, read_pkt_line, trim_trailing_newline, write_flush, write_pkt_line,
};

/// The result of a wire fetch (`upload-pack`) over the local transport.
#[derive(Debug, Clone)]
pub struct WireFetch {
    /// The refs the server advertised (peeled `^{}` entries dropped), name-sorted
    /// — `HEAD` plus every branch/tag the remote offered.
    pub refs: Vec<(String, GitOid)>,
    /// The full import result: per-type counts and the git→omoplata oid mapping,
    /// exactly as [`crate::import_repo`] produces for an on-disk repo.
    pub import: GitImport,
    /// The number of raw packfile bytes received over the wire (including the
    /// 12-byte header and 20-byte trailing checksum).
    pub pack_bytes: usize,
}

/// Clone every advertised ref of a local git repository over the wire, importing
/// the received packfile into `repo`.
///
/// `url_or_path` may be a `file://` URL or a plain local path pointing at either
/// a working tree (git finds its `.git`) or a git directory. The full object
/// graph reachable from the advertised refs is fetched (a full, non-thin clone),
/// decoded, and imported through the I9 round-trip gate.
///
/// # Errors
/// Returns [`GitError::Wire`] if `git upload-pack` (or `git-upload-pack`) cannot
/// be spawned or exits non-zero, [`GitError::WireProto`] on a malformed
/// advertisement or an unexpected response, [`GitError::Pack`] on a malformed
/// packfile, or any error from the I9 gate / store import.
pub fn fetch_local(url_or_path: &str, repo: &Repository) -> Result<WireFetch, GitError> {
    let dir = resolve_local_path(url_or_path);

    let mut child = spawn_upload_pack(&dir)?;

    // Take the child's stdio handles. `stdin`/`stdout` are always `Some` here
    // because we configured both as piped when spawning.
    let mut stdin = child
        .stdin
        .take()
        .ok_or(GitError::Wire("upload-pack stdin was not piped".to_owned()))?;
    let stdout = child.stdout.take().ok_or(GitError::Wire(
        "upload-pack stdout was not piped".to_owned(),
    ))?;
    let mut reader = BufReader::new(stdout);

    // Step 2: read the ref advertisement (refs + the server's capabilities).
    let (advertised, server_caps) = read_advertisement(&mut reader)?;

    // Choose the wants: the distinct oids of the advertised refs (a full clone).
    let mut want_oids: Vec<GitOid> = Vec::new();
    let mut seen: HashSet<GitOid> = HashSet::new();
    for (_name, oid) in &advertised {
        if seen.insert(*oid) {
            want_oids.push(*oid);
        }
    }

    // An empty repository advertises no real refs; there is nothing to fetch.
    if want_oids.is_empty() {
        // Politely end the want section so upload-pack exits cleanly.
        write_flush(&mut stdin).map_err(pass)?;
        drop(stdin);
        reap(child)?;
        let import = import_objects(&Default::default(), advertised.clone(), repo)?;
        return Ok(WireFetch {
            refs: advertised,
            import,
            pack_bytes: 0,
        });
    }

    // Step 3: send want lines, a flush, then done.
    let caps = negotiated_capabilities(&server_caps);
    send_wants(&mut stdin, &want_oids, &caps)?;
    // No `have` lines (a full clone knows nothing); a `done` line ends
    // negotiation and asks the server to send the pack immediately.
    write_pkt_line(&mut stdin, b"done\n").map_err(pass)?;
    stdin.flush().map_err(|e| GitError::Wire(e.to_string()))?;
    // Close stdin so the server sees end-of-input.
    drop(stdin);

    // Step 4: read the NAK acknowledgement, then the raw packfile to EOF.
    read_until_nak(&mut reader)?;
    let mut pack = Vec::new();
    reader
        .read_to_end(&mut pack)
        .map_err(|e| GitError::Wire(e.to_string()))?;

    reap(child)?;

    if pack.is_empty() {
        return Err(GitError::WireProto("server sent an empty packfile"));
    }
    let pack_bytes = pack.len();

    // Step 5: decode the in-memory pack and import through the I9 gate.
    let mut objects = std::collections::HashMap::new();
    for (oid, object) in parse_pack_bytes(&pack)? {
        objects.entry(oid).or_insert(object);
    }
    let import = import_objects(&objects, advertised.clone(), repo)?;

    Ok(WireFetch {
        refs: advertised,
        import,
        pack_bytes,
    })
}

/// Resolve a `file://` URL or a local path to a filesystem path for the repo dir.
fn resolve_local_path(url_or_path: &str) -> PathBuf {
    if let Some(rest) = url_or_path.strip_prefix("file://") {
        // `file:///abs/path` → an authority-less URL; strip a leading `localhost`
        // authority if present, otherwise the remainder is already the path.
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        PathBuf::from(rest)
    } else {
        PathBuf::from(url_or_path)
    }
}

/// Spawn `git upload-pack <dir>` (falling back to the `git-upload-pack` binary),
/// with stdin/stdout piped and stderr discarded.
fn spawn_upload_pack(dir: &std::path::Path) -> Result<std::process::Child, GitError> {
    // Ensure protocol **v0**: never let an inherited `GIT_PROTOCOL=version=2`
    // change the framing this client speaks.
    let subcommand = Command::new("git")
        .arg("upload-pack")
        .arg(dir)
        .env_remove("GIT_PROTOCOL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    match subcommand {
        Ok(child) => Ok(child),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Some environments only ship the dashed helper on PATH.
            Command::new("git-upload-pack")
                .arg(dir)
                .env_remove("GIT_PROTOCOL")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e2| {
                    GitError::Wire(format!(
                        "could not spawn `git upload-pack` or `git-upload-pack`: {e2}"
                    ))
                })
        }
        Err(e) => Err(GitError::Wire(format!(
            "could not spawn `git upload-pack`: {e}"
        ))),
    }
}

/// The parsed ref advertisement: `(name-sorted refs, server capabilities)`.
type Advertisement = (Vec<(String, GitOid)>, HashSet<String>);

/// Read the ref advertisement: a run of `"<oid> <ref>"` pkt-lines terminated by a
/// flush-pkt. The first line additionally carries `"\0<capabilities>"`. Peeled
/// (`^{}`) tag lines and the empty-repo `capabilities^{}` placeholder are
/// dropped. Returns `(name-sorted refs, server capabilities)`.
fn read_advertisement<R: Read>(reader: &mut R) -> Result<Advertisement, GitError> {
    let mut refs: Vec<(String, GitOid)> = Vec::new();
    let mut caps: HashSet<String> = HashSet::new();
    let mut first = true;
    while let Some(line) = read_pkt_line(reader)? {
        let line = trim_trailing_newline(&line);
        // A stray protocol banner (e.g. "version 1") is not a ref line — skip it.
        if !looks_like_ref_line(line) {
            first = false;
            continue;
        }
        // On the first advertised ref the capabilities follow a NUL byte.
        let (ref_part, cap_part) = split_capabilities(line, first);
        first = false;
        if let Some(cap_bytes) = cap_part {
            for cap in String::from_utf8_lossy(cap_bytes).split_whitespace() {
                caps.insert(cap.to_owned());
            }
        }
        let (oid, name) = parse_ref_line(ref_part)?;
        // Drop peeled tag lines and the empty-repo placeholder ref.
        if name.ends_with("^{}") || name == "capabilities^{}" {
            continue;
        }
        refs.push((name, oid));
    }
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((refs, caps))
}

/// Whether `line` begins with 40 hex characters followed by a space — the shape
/// of a ref-advertisement line (as opposed to a protocol banner).
fn looks_like_ref_line(line: &[u8]) -> bool {
    line.len() > 40 && line[40] == b' ' && line[..40].iter().all(u8::is_ascii_hexdigit)
}

/// Split a first-line advertisement into `(ref-bytes, Some(cap-bytes))` at the
/// NUL, or `(line, None)` for subsequent lines / a first line without a NUL.
fn split_capabilities(line: &[u8], first: bool) -> (&[u8], Option<&[u8]>) {
    if !first {
        return (line, None);
    }
    match line.iter().position(|&b| b == 0) {
        Some(nul) => (&line[..nul], Some(&line[nul + 1..])),
        None => (line, None),
    }
}

/// Choose the capability set to request. We ask for `ofs-delta` when the server
/// offers it (compact packs, offset-based deltas the in-memory decoder handles),
/// and deliberately omit `side-band`/`side-band-64k` so the packfile arrives raw.
fn negotiated_capabilities(server_caps: &HashSet<String>) -> Vec<&'static str> {
    let mut caps = Vec::new();
    if server_caps.contains("ofs-delta") {
        caps.push("ofs-delta");
    }
    caps
}

/// Send the `want` lines: capabilities ride on the first line only, per protocol.
fn send_wants<W: Write>(stdin: &mut W, wants: &[GitOid], caps: &[&str]) -> Result<(), GitError> {
    for (i, oid) in wants.iter().enumerate() {
        let line = if i == 0 && !caps.is_empty() {
            format!("want {} {}\n", oid.hex(), caps.join(" "))
        } else {
            format!("want {}\n", oid.hex())
        };
        write_pkt_line(stdin, line.as_bytes()).map_err(pass)?;
    }
    // A flush-pkt ends the want list.
    write_flush(stdin).map_err(pass)
}

/// Consume acknowledgement pkt-lines until the terminating `NAK` (a full clone
/// with no `have` lines always gets exactly `NAK`). `ACK …` lines (should a
/// server send them) are tolerated and skipped.
fn read_until_nak<R: Read>(reader: &mut R) -> Result<(), GitError> {
    loop {
        match read_pkt_line(reader)? {
            Some(line) => {
                let text = trim_trailing_newline(&line);
                if text == b"NAK" {
                    return Ok(());
                }
                if text.starts_with(b"ACK") {
                    // Common-object acknowledgement; keep reading until NAK/pack.
                    continue;
                }
                return Err(GitError::WireProto(
                    "unexpected pkt-line before packfile (expected NAK)",
                ));
            }
            None => {
                return Err(GitError::WireProto(
                    "stream ended before NAK acknowledgement",
                ));
            }
        }
    }
}

/// Wait for the `upload-pack` child and turn a non-zero exit into an error.
fn reap(mut child: std::process::Child) -> Result<(), GitError> {
    let status = child
        .wait()
        .map_err(|e| GitError::Wire(format!("waiting for upload-pack: {e}")))?;
    if !status.success() {
        return Err(GitError::Wire(format!("upload-pack exited with {status}")));
    }
    Ok(())
}

/// Map a pkt-line write error into the transport error space. (`write_pkt_line`
/// already returns [`GitError`]; this is the identity, kept for call-site
/// readability.)
fn pass(e: GitError) -> GitError {
    e
}

/// A convenience for callers/tests that only want the imported [`GitObject`]s
/// decoded from a raw packfile without spawning a process — exercises the exact
/// in-memory decode path [`fetch_local`] uses.
///
/// # Errors
/// Propagates any [`GitError`] from [`parse_pack_bytes`].
pub fn decode_wire_pack(pack: &[u8]) -> Result<Vec<(GitOid, GitObject)>, GitError> {
    parse_pack_bytes(pack)
}
