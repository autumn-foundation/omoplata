//! Networked transport for omoplata remotes (ADR-0010, Phase: networked reach).
//!
//! A minimal, dependency-free HTTP/1.1 client and server over [`std::net`],
//! plus the omoplata sync protocol. This is the local-path transport lifted onto
//! a socket: `omo serve` exposes a repository as a **landing authority** off-box,
//! and `omo fetch` / `omo push` speak to it over `http://`.
//!
//! Scope: HTTP only, no TLS — TLS is terminated by a fronting proxy/tunnel
//! (see ADR-0010), not built in. Authentication is an optional bearer token:
//! `omo serve --token <t>` requires `Authorization: Bearer <t>` on every request
//! and answers `401` otherwise; an open server (no token) accepts all comers, for
//! loopback / trusted networks.
//!
//! Protocol (all bodies are UTF-8):
//! * `GET /refs` → JSON `{ "<ref>": "<commit>", … }` — the queue-visible refs
//!   (`public/*` and `reconciled/*`).
//! * `GET /object/<id>` → the object's canonical bytes (octet-stream), or 404.
//! * `POST /push` → a [`PushPayload`] JSON; the server writes the objects,
//!   records the change tips, and lands the submission through *its* policy,
//!   returning a [`PushResult`].

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use omoplata_store::{Object, ObjectId, Repository};
use omoplata_work::OpLog;

/// A parsed HTTP response.
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// If `remote` is an `http://host:port` URL, return `host:port`; else `None`
/// (the caller falls back to the local-path transport). Any trailing path is
/// rejected — the MVP serves a single repo at the root.
#[must_use]
pub fn http_authority(remote: &str) -> Option<String> {
    let rest = remote.strip_prefix("http://")?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        None
    } else {
        Some(authority.to_owned())
    }
}

/// `GET path` against `authority` (`host:port`), optionally bearer-authenticated.
pub fn get(authority: &str, path: &str, token: Option<&str>) -> anyhow::Result<HttpResponse> {
    request(authority, "GET", path, &[], token)
}

/// `POST path` with `body` against `authority`, optionally bearer-authenticated.
pub fn post(
    authority: &str,
    path: &str,
    body: &[u8],
    token: Option<&str>,
) -> anyhow::Result<HttpResponse> {
    request(authority, "POST", path, body, token)
}

fn request(
    authority: &str,
    method: &str,
    path: &str,
    body: &[u8],
    token: Option<&str>,
) -> anyhow::Result<HttpResponse> {
    let mut stream =
        TcpStream::connect(authority).with_context(|| format!("connecting to {authority}"))?;
    let auth = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\n{auth}Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> anyhow::Result<HttpResponse> {
    let split =
        find_header_end(raw).ok_or_else(|| anyhow!("malformed HTTP response (no header end)"))?;
    let head = std::str::from_utf8(&raw[..split]).context("non-utf8 response head")?;
    let status_line = head.lines().next().unwrap_or("");
    // "HTTP/1.1 200 OK"
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("malformed status line: {status_line:?}"))?;
    let body = raw[split + 4..].to_vec();
    Ok(HttpResponse { status, body })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|w| w == b"\r\n\r\n")
}

// --- protocol payloads -------------------------------------------------------

/// The body of a `POST /push`: a submission to land on the server's queue, the
/// change tips it references, and the object closure the server may be missing.
#[derive(Debug, Serialize, Deserialize)]
pub struct PushPayload {
    /// Target queue on the server (default `trunk`).
    pub queue: String,
    /// The submission (its approval and certificates travel with it).
    pub submission: omoplata_identity::Submission,
    /// `(change id, tip commit)` for each change the submission references.
    pub tips: Vec<(String, String)>,
    /// `(object id, hex-encoded canonical bytes)` for the pushed closure.
    pub objects: Vec<(String, String)>,
}

/// The result of a `POST /push`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PushResult {
    pub ok: bool,
    pub message: String,
}

// --- client operations -------------------------------------------------------

/// Fetch the server's queue-visible refs (`name -> commit`).
pub fn fetch_refs(
    authority: &str,
    token: Option<&str>,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let resp = get(authority, "/refs", token)?;
    if resp.status != 200 {
        anyhow::bail!("remote {authority}: GET /refs returned {}", resp.status);
    }
    serde_json::from_slice(&resp.body).context("decoding /refs response")
}

/// Fetch one object's canonical bytes by id.
pub fn fetch_object(authority: &str, id: &str, token: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let resp = get(authority, &format!("/object/{id}"), token)?;
    if resp.status != 200 {
        anyhow::bail!(
            "remote {authority}: GET /object/{id} returned {}",
            resp.status
        );
    }
    Ok(resp.body)
}

/// Push a payload; returns the server's result (its land message, or an error).
pub fn push(
    authority: &str,
    payload: &PushPayload,
    token: Option<&str>,
) -> anyhow::Result<PushResult> {
    let body = serde_json::to_vec(payload)?;
    let resp = post(authority, "/push", &body, token)?;
    // The server encodes application-level refusals as `{ok:false}` with 200;
    // a non-200 is a transport/parse failure.
    if resp.status != 200 {
        anyhow::bail!(
            "remote {authority}: POST /push returned {} ({})",
            resp.status,
            String::from_utf8_lossy(&resp.body).trim()
        );
    }
    serde_json::from_slice(&resp.body).context("decoding /push response")
}

// --- server ------------------------------------------------------------------

/// Serve `repo_path` over HTTP at `addr` until interrupted. Prints the bound
/// address (useful when `addr` ends in `:0`). One thread per connection; writes
/// serialize on the repository's advisory lock, so concurrent pushes are safe.
///
/// If `token` is `Some`, every request must carry `Authorization: Bearer <token>`
/// or it is answered `401`; `None` serves openly (loopback / trusted network).
pub fn serve(repo_path: PathBuf, addr: &str, token: Option<String>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).with_context(|| format!("binding {addr}"))?;
    let bound = listener.local_addr()?;
    let auth = if token.is_some() {
        " (authenticated)"
    } else {
        ""
    };
    println!("omo serve: {} on http://{bound}{auth}", repo_path.display());
    // Flush so a parent process reading stdout sees the address immediately.
    std::io::stdout().flush().ok();

    let token = std::sync::Arc::new(token);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let repo_path = repo_path.clone();
                let token = std::sync::Arc::clone(&token);
                std::thread::spawn(move || {
                    if let Err(e) = handle(&stream, &repo_path, token.as_deref()) {
                        let _ = write_response(&stream, 500, b"", &e.to_string());
                    }
                });
            }
            Err(e) => eprintln!("omo serve: accept error: {e}"),
        }
    }
    Ok(())
}

fn handle(
    stream: &TcpStream,
    repo_path: &std::path::Path,
    token: Option<&str>,
) -> anyhow::Result<()> {
    let (method, path, bearer, body) = read_request(stream)?;
    // Bearer-token gate: when the server was started with a token, every request
    // must present it. Unauthenticated servers (`token == None`) skip the check.
    if let Some(expected) = token {
        if bearer.as_deref() != Some(expected) {
            return write_response(stream, 401, b"", "Unauthorized");
        }
    }
    match (method.as_str(), path.as_str()) {
        ("GET", "/refs") => {
            let refs = serve_refs(repo_path)?;
            let body = serde_json::to_vec(&refs)?;
            write_response(stream, 200, &body, "OK")
        }
        ("GET", p) if p.starts_with("/object/") => {
            let id = &p["/object/".len()..];
            match serve_object(repo_path, id) {
                Ok(Some(bytes)) => write_response(stream, 200, &bytes, "OK"),
                Ok(None) => write_response(stream, 404, b"", "Not Found"),
                Err(e) => write_response(stream, 500, e.to_string().as_bytes(), "Error"),
            }
        }
        ("POST", "/push") => {
            let result = match serde_json::from_slice::<PushPayload>(&body)
                .context("decoding push payload")
                .and_then(|payload| crate::accept_push(repo_path, payload))
            {
                Ok(message) => PushResult { ok: true, message },
                Err(e) => PushResult {
                    ok: false,
                    message: e.to_string(),
                },
            };
            let body = serde_json::to_vec(&result)?;
            write_response(stream, 200, &body, "OK")
        }
        _ => write_response(stream, 404, b"", "Not Found"),
    }
}

/// The queue-visible refs a client may fetch: landed (`public/*`) and the
/// merged-trunk heads (`reconciled/*`). Private `ws/*` tips are never served.
fn serve_refs(
    repo_path: &std::path::Path,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let repo = Repository::open(repo_path)?;
    let refs = OpLog::load(crate::oplog_path(&repo))?.refs_now();
    Ok(refs
        .into_iter()
        .filter(|(k, _)| k.starts_with("public/") || k.starts_with("reconciled/"))
        .map(|(k, v)| (k, v.to_string()))
        .collect())
}

/// The canonical bytes of an object, or `None` if the store does not have it.
fn serve_object(repo_path: &std::path::Path, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let repo = Repository::open(repo_path)?;
    let Ok(oid) = id.parse::<ObjectId>() else {
        return Ok(None);
    };
    if !repo.has_object(&oid) {
        return Ok(None);
    }
    Ok(Some(repo.read_object(&oid)?.serialize()))
}

/// Parse a request into `(method, path, bearer-token, body)`. The bearer token
/// is the value after `Authorization: Bearer `, if that header is present.
fn read_request(
    mut stream: &TcpStream,
) -> anyhow::Result<(String, String, Option<String>, Vec<u8>)> {
    // Read until the header terminator, then the Content-Length body.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(anyhow!("connection closed before headers completed"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = std::str::from_utf8(&buf[..header_end]).context("non-utf8 request head")?;
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();

    let mut content_length = 0usize;
    let mut bearer = None;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim();
        if k.eq_ignore_ascii_case("content-length") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if k.eq_ignore_ascii_case("authorization") {
            bearer = v
                .trim()
                .strip_prefix("Bearer ")
                .map(|t| t.trim().to_owned());
        }
    }

    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    Ok((method, path, bearer, body))
}

fn write_response(
    mut stream: &TcpStream,
    status: u16,
    body: &[u8],
    reason: &str,
) -> anyhow::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

// --- hex (dependency-free object framing over JSON) --------------------------

/// Lower-case hex encoding of `bytes`.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    out
}

/// Decode a lower/upper-case hex string into bytes.
pub fn from_hex(s: &str) -> anyhow::Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        anyhow::bail!("odd-length hex string");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char)
            .to_digit(16)
            .ok_or_else(|| anyhow!("invalid hex digit"))?;
        let lo = (bytes[i + 1] as char)
            .to_digit(16)
            .ok_or_else(|| anyhow!("invalid hex digit"))?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Ok(out)
}

/// Parse `(id, hex)` back into an [`Object`] (the id is re-derived on write, so
/// content-addressing self-verifies).
pub fn object_from_hex(hex: &str) -> anyhow::Result<Object> {
    let bytes = from_hex(hex)?;
    Object::deserialize(&bytes).map_err(|e| anyhow!("decoding object: {e}"))
}
