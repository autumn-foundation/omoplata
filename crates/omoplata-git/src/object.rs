//! Git object model and a byte-faithful codec.
//!
//! Git addresses an object by the SHA-1 of its *uncompressed* serialized form
//! `"{type} {len}\0{body}"`, and stores that form zlib-compressed as a loose
//! file. This module models the four git object types and provides
//! [`encode`]/[`decode`]/[`oid`] such that a decoded object re-encodes
//! **byte-identically** — the property the round-trip gate (I9) depends on.

use std::fmt;
use std::str::FromStr;

use sha1::{Digest, Sha1};

use crate::error::GitError;

/// A git object identity: the 20-byte SHA-1 of the encoded object.
///
/// Displayed and parsed as 40 lowercase hex characters, matching the name git
/// uses for the loose-object path `<xx>/<38 hex>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GitOid([u8; 20]);

impl GitOid {
    /// Wrap 20 raw SHA-1 bytes as an oid.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        GitOid(bytes)
    }

    /// The raw 20 SHA-1 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Lowercase 40-character hex form.
    #[must_use]
    pub fn hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Parse a 40-character lowercase hex string into an oid.
    ///
    /// # Errors
    /// Returns [`GitError::MalformedOid`] if the input is not exactly 40 hex
    /// characters.
    pub fn from_hex(s: &str) -> Result<Self, GitError> {
        let bytes = hex_decode(s).ok_or_else(|| GitError::MalformedOid(s.to_owned()))?;
        let arr: [u8; 20] = bytes
            .try_into()
            .map_err(|_| GitError::MalformedOid(s.to_owned()))?;
        Ok(GitOid(arr))
    }
}

impl fmt::Display for GitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl FromStr for GitOid {
    type Err = GitError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        GitOid::from_hex(s)
    }
}

/// A single entry in a git tree object.
///
/// Git stores each entry as `"{mode} {name}\0"` followed by the referenced
/// object's 20 raw SHA-1 bytes. The `mode` is kept as the exact ASCII string
/// git wrote (e.g. `"100644"`, `"100755"`, `"120000"`, `"40000"`) so that
/// re-encoding is byte-identical — git does *not* zero-pad the tree mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeEntry {
    /// The octal file mode, exactly as stored (no zero-padding normalization).
    pub mode: String,
    /// The entry name (a single path component).
    pub name: String,
    /// The referenced object's raw SHA-1.
    pub oid: [u8; 20],
}

/// A parsed git commit object.
///
/// The graph-relevant fields git commits carry — `tree`, zero-or-more
/// `parents`, `author`, `committer`, and the free-text `message` — are modelled
/// as typed fields so the commit graph can be walked (M10). The original body
/// bytes are **also retained** in [`GitCommit::body`] so that re-encoding is
/// byte-identical regardless of any header this struct does not model — extra
/// or reordered headers, `gpgsig`/`mergetag`/`encoding`, and the exact
/// timestamp/timezone formatting of the identity lines. Retaining the raw body
/// is the design doc's sanctioned move for fields too fiddly to model losslessly
/// (§3 P8's byte-identity gate over faithful modelling): the typed view is real
/// and drives the DAG, while byte-identity leans on the untouched bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    /// The root tree this commit snapshots.
    pub tree: GitOid,
    /// The parent commits, in header order (empty for a root commit; one for a
    /// normal commit; two or more for a merge).
    pub parents: Vec<GitOid>,
    /// The `author` identity line, verbatim after the `author ` prefix
    /// (`Name <email> <unix-ts> <tz>`).
    pub author: String,
    /// The `committer` identity line, verbatim after the `committer ` prefix.
    pub committer: String,
    /// The commit message: every byte after the blank line that separates the
    /// header block from the message, decoded lossily as UTF-8 for display.
    pub message: String,
    /// The exact original body bytes, retained for byte-identical re-encoding.
    body: Vec<u8>,
}

impl GitCommit {
    /// Parse a commit object's body into a [`GitCommit`].
    ///
    /// Extracts the typed fields (`tree`, `parents`, `author`, `committer`,
    /// `message`) while retaining `body` verbatim for exact re-encoding.
    ///
    /// # Errors
    /// Returns [`GitError::Decode`] if the header block is not valid UTF-8, if a
    /// `tree`/`parent` header does not carry a 40-hex oid, or if the mandatory
    /// `tree` header is absent.
    pub fn parse(body: &[u8]) -> Result<Self, GitError> {
        let (header_block, message_bytes) = split_headers(body);
        let headers = parse_headers(header_block)?;
        let mut tree: Option<GitOid> = None;
        let mut parents = Vec::new();
        let mut author = String::new();
        let mut committer = String::new();
        for (key, value) in &headers {
            match key.as_str() {
                "tree" => {
                    tree =
                        Some(GitOid::from_hex(value.trim()).map_err(|_| {
                            GitError::Decode("commit tree header is not a 40-hex oid")
                        })?);
                }
                "parent" => {
                    parents.push(GitOid::from_hex(value.trim()).map_err(|_| {
                        GitError::Decode("commit parent header is not a 40-hex oid")
                    })?)
                }
                "author" => author = value.clone(),
                "committer" => committer = value.clone(),
                _ => {}
            }
        }
        let tree = tree.ok_or(GitError::Decode("commit missing tree header"))?;
        let message = String::from_utf8_lossy(message_bytes).into_owned();
        Ok(GitCommit {
            tree,
            parents,
            author,
            committer,
            message,
            body: body.to_vec(),
        })
    }

    /// The exact body bytes this commit re-encodes to (without the object
    /// header). Retained so re-encoding is byte-identical.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The first line of the message — the commit subject.
    #[must_use]
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }
}

/// A parsed annotated-tag object.
///
/// As with [`GitCommit`], the graph-relevant fields are typed (`object` and its
/// `kind`, the `tag` name, the `tagger` line, and the `message`) while the raw
/// `body` is retained so a signed tag's `gpgsig`-in-message and exact byte
/// layout re-encode identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTag {
    /// The oid of the object this tag points at (usually a commit).
    pub object: GitOid,
    /// The target object's type keyword (`commit`, `tree`, `blob`, or `tag`).
    pub kind: String,
    /// The tag name (e.g. `v1.0`).
    pub tag: String,
    /// The `tagger` identity line, verbatim after the `tagger ` prefix. Empty
    /// if the tag omits it.
    pub tagger: String,
    /// The tag message: bytes after the blank line, decoded lossily as UTF-8.
    pub message: String,
    /// The exact original body bytes, retained for byte-identical re-encoding.
    body: Vec<u8>,
}

impl GitTag {
    /// Parse a tag object's body into a [`GitTag`].
    ///
    /// # Errors
    /// Returns [`GitError::Decode`] if the header block is not valid UTF-8, if
    /// the `object` header does not carry a 40-hex oid, or if the mandatory
    /// `object` header is absent.
    pub fn parse(body: &[u8]) -> Result<Self, GitError> {
        let (header_block, message_bytes) = split_headers(body);
        let headers = parse_headers(header_block)?;
        let mut object: Option<GitOid> = None;
        let mut kind = String::new();
        let mut tag = String::new();
        let mut tagger = String::new();
        for (key, value) in &headers {
            match key.as_str() {
                "object" => {
                    object =
                        Some(GitOid::from_hex(value.trim()).map_err(|_| {
                            GitError::Decode("tag object header is not a 40-hex oid")
                        })?);
                }
                "type" => kind = value.clone(),
                "tag" => tag = value.clone(),
                "tagger" => tagger = value.clone(),
                _ => {}
            }
        }
        let object = object.ok_or(GitError::Decode("tag missing object header"))?;
        let message = String::from_utf8_lossy(message_bytes).into_owned();
        Ok(GitTag {
            object,
            kind,
            tag,
            tagger,
            message,
            body: body.to_vec(),
        })
    }

    /// The exact body bytes this tag re-encodes to (without the object header).
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Split a commit/tag body into its header block and message.
///
/// The header block is everything before the first empty line (`"\n\n"`); the
/// message is everything after it. Continuation lines inside a header value
/// (`gpgsig`, `mergetag`) begin with a space and never contain an empty line,
/// so the first `"\n\n"` is unambiguously the header/message boundary.
fn split_headers(body: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i + 1 < body.len() {
        if body[i] == b'\n' && body[i + 1] == b'\n' {
            return (&body[..i], &body[i + 2..]);
        }
        i += 1;
    }
    (body, &[])
}

/// Parse a header block into ordered `(key, value)` pairs, folding continuation
/// lines (those beginning with a space) into the preceding header's value with a
/// `"\n"` join. Only used to extract typed fields; byte-identity relies on the
/// retained raw body, not on this parse being reversible.
fn parse_headers(block: &[u8]) -> Result<Vec<(String, String)>, GitError> {
    let text =
        std::str::from_utf8(block).map_err(|_| GitError::Decode("non-utf8 commit/tag header"))?;
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        if let Some(cont) = line.strip_prefix(' ') {
            if let Some(last) = headers.last_mut() {
                last.1.push('\n');
                last.1.push_str(cont);
            }
            continue;
        }
        match line.split_once(' ') {
            Some((k, v)) => headers.push((k.to_owned(), v.to_owned())),
            None => headers.push((line.to_owned(), String::new())),
        }
    }
    Ok(headers)
}

/// A parsed git object.
///
/// Blobs and trees carry structured content; commits and tags carry typed
/// graph fields ([`GitCommit`], [`GitTag`]) plus their retained raw body, so
/// every variant re-encodes byte-identically (the I9 gate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitObject {
    /// Opaque file content.
    Blob(Vec<u8>),
    /// A directory: an ordered list of tree entries.
    Tree(Vec<GitTreeEntry>),
    /// A commit, with typed graph fields and its retained raw body.
    Commit(GitCommit),
    /// An annotated tag, with typed fields and its retained raw body.
    Tag(GitTag),
}

impl GitObject {
    /// The git type keyword for this object (`blob`, `tree`, `commit`, `tag`).
    #[must_use]
    pub fn type_str(&self) -> &'static str {
        match self {
            GitObject::Blob(_) => "blob",
            GitObject::Tree(_) => "tree",
            GitObject::Commit(_) => "commit",
            GitObject::Tag(_) => "tag",
        }
    }
}

/// Encode an object to git's uncompressed serialized form
/// `"{type} {len}\0{body}"`.
///
/// This is the exact byte sequence git hashes with SHA-1 and, once
/// zlib-compressed, writes as a loose object. Trees re-encode byte-identically:
/// mode strings, entry order, names, and raw oids are all preserved.
#[must_use]
pub fn encode(object: &GitObject) -> Vec<u8> {
    let body = match object {
        GitObject::Blob(b) => b.clone(),
        GitObject::Tree(entries) => encode_tree(entries),
        GitObject::Commit(c) => c.body.clone(),
        GitObject::Tag(t) => t.body.clone(),
    };
    let ty = object.type_str();
    let len = body.len().to_string();
    let mut out = Vec::with_capacity(ty.len() + 1 + len.len() + 1 + body.len());
    out.extend_from_slice(ty.as_bytes());
    out.push(b' ');
    out.extend_from_slice(len.as_bytes());
    out.push(0);
    out.extend_from_slice(&body);
    out
}

fn encode_tree(entries: &[GitTreeEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(e.mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(e.name.as_bytes());
        out.push(0);
        out.extend_from_slice(&e.oid);
    }
    out
}

/// Decode git's uncompressed serialized form into a [`GitObject`].
///
/// # Errors
/// Returns [`GitError::Decode`] if the header, declared length, object type, or
/// tree body is malformed.
pub fn decode(bytes: &[u8]) -> Result<GitObject, GitError> {
    let sp = bytes
        .iter()
        .position(|&b| b == b' ')
        .ok_or(GitError::Decode("missing type/size separator"))?;
    let ty = &bytes[..sp];
    let nul = bytes[sp + 1..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| sp + 1 + i)
        .ok_or(GitError::Decode("missing header NUL"))?;
    let size_s =
        std::str::from_utf8(&bytes[sp + 1..nul]).map_err(|_| GitError::Decode("non-utf8 size"))?;
    let size: usize = size_s.parse().map_err(|_| GitError::Decode("bad size"))?;
    let body = &bytes[nul + 1..];
    if body.len() != size {
        return Err(GitError::Decode("declared size does not match body length"));
    }
    match ty {
        b"blob" => Ok(GitObject::Blob(body.to_vec())),
        b"tree" => Ok(GitObject::Tree(decode_tree(body)?)),
        b"commit" => Ok(GitObject::Commit(GitCommit::parse(body)?)),
        b"tag" => Ok(GitObject::Tag(GitTag::parse(body)?)),
        _ => Err(GitError::Decode("unknown object type")),
    }
}

fn decode_tree(body: &[u8]) -> Result<Vec<GitTreeEntry>, GitError> {
    let mut entries = Vec::new();
    let mut i = 0usize;
    while i < body.len() {
        let sp = body[i..]
            .iter()
            .position(|&b| b == b' ')
            .map(|p| i + p)
            .ok_or(GitError::Decode("tree entry: missing mode separator"))?;
        let mode = std::str::from_utf8(&body[i..sp])
            .map_err(|_| GitError::Decode("tree entry: non-utf8 mode"))?
            .to_owned();
        let nul = body[sp + 1..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| sp + 1 + p)
            .ok_or(GitError::Decode("tree entry: missing name NUL"))?;
        let name = std::str::from_utf8(&body[sp + 1..nul])
            .map_err(|_| GitError::Decode("tree entry: non-utf8 name"))?
            .to_owned();
        let oid_start = nul + 1;
        let oid_end = oid_start
            .checked_add(20)
            .ok_or(GitError::Decode("tree entry: oid offset overflow"))?;
        if oid_end > body.len() {
            return Err(GitError::Decode("tree entry: truncated oid"));
        }
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&body[oid_start..oid_end]);
        entries.push(GitTreeEntry { mode, name, oid });
        i = oid_end;
    }
    Ok(entries)
}

/// The git oid (SHA-1 of the encoded form) of an object.
#[must_use]
pub fn oid(object: &GitObject) -> GitOid {
    GitOid(sha1_bytes(&encode(object)))
}

/// SHA-1 of arbitrary bytes as a 20-byte array.
pub(crate) fn sha1_bytes(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&digest);
    arr
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git's canonical empty-blob oid.
    const EMPTY_BLOB_OID: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    #[test]
    fn empty_blob_oid_matches_git() {
        let oid = oid(&GitObject::Blob(Vec::new()));
        assert_eq!(oid.hex(), EMPTY_BLOB_OID);
    }

    #[test]
    fn hello_blob_oid_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin` == this constant.
        let oid = oid(&GitObject::Blob(b"hello\n".to_vec()));
        assert_eq!(oid.hex(), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn blob_encode_has_git_header() {
        let encoded = encode(&GitObject::Blob(b"hello\n".to_vec()));
        assert_eq!(&encoded, b"blob 6\0hello\n");
    }

    #[test]
    fn blob_roundtrip() {
        let obj = GitObject::Blob(b"arbitrary\0bytes\xff".to_vec());
        assert_eq!(decode(&encode(&obj)).unwrap(), obj);
    }

    #[test]
    fn tree_roundtrips_byte_identically_and_preserves_order() {
        // A blob entry (100644) sorted before a subtree entry (40000). Git
        // orders tree entries by name; "data" < "src".
        let blob_oid = oid(&GitObject::Blob(b"x".to_vec())).0;
        let sub_oid = oid(&GitObject::Tree(Vec::new())).0;
        let tree = GitObject::Tree(vec![
            GitTreeEntry {
                mode: "100644".to_owned(),
                name: "data".to_owned(),
                oid: blob_oid,
            },
            GitTreeEntry {
                mode: "40000".to_owned(),
                name: "src".to_owned(),
                oid: sub_oid,
            },
        ]);
        let once = encode(&tree);
        let twice = encode(&decode(&once).unwrap());
        assert_eq!(once, twice, "tree must re-encode byte-identically");

        // Name ordering is preserved through decode.
        let GitObject::Tree(entries) = decode(&once).unwrap() else {
            panic!("expected tree");
        };
        assert_eq!(entries[0].name, "data");
        assert_eq!(entries[1].name, "src");
        assert_eq!(entries[1].mode, "40000");
    }

    #[test]
    fn oid_hex_roundtrips() {
        let o = oid(&GitObject::Blob(b"hello\n".to_vec()));
        assert_eq!(GitOid::from_hex(&o.hex()).unwrap(), o);
        assert_eq!(o.to_string().parse::<GitOid>().unwrap(), o);
    }

    #[test]
    fn from_hex_rejects_bad_input() {
        assert!(GitOid::from_hex("").is_err());
        assert!(GitOid::from_hex("abc").is_err()); // odd length
        assert!(GitOid::from_hex(&"zz".repeat(20)).is_err()); // non-hex
        assert!(GitOid::from_hex(&"ab".repeat(19)).is_err()); // too short
    }

    #[test]
    fn decode_rejects_malformed() {
        assert!(decode(b"blob 99\0hi").is_err()); // length mismatch
        assert!(decode(b"no-space").is_err());
        assert!(decode(b"widget 0\0").is_err()); // unknown type
    }

    /// A root commit body (no parent) with a trailing-newline message.
    const ROOT_COMMIT_BODY: &[u8] = b"tree 717a800ec7075fc0c5803b2683402dcfcd38fcff\n\
          author T E <t@e.dev> 1784708656 +0000\n\
          committer T E <t@e.dev> 1784708656 +0000\n\
          \n\
          first commit\n";

    /// A child commit with one parent and a multi-line message.
    const CHILD_COMMIT_BODY: &[u8] = b"tree aa6015803596511f11590d0ab6f7923a69007b5d\n\
          parent 57d3fe268dca377f8592f29a3fcd9cae7b23e49d\n\
          author T E <t@e.dev> 1784708700 +0000\n\
          committer T E <t@e.dev> 1784708700 +0000\n\
          \n\
          second commit\n\nbody line\n";

    #[test]
    fn commit_parses_typed_fields() {
        let GitObject::Commit(c) = decode(&encode(&GitObject::Commit(
            GitCommit::parse(ROOT_COMMIT_BODY).unwrap(),
        )))
        .unwrap() else {
            panic!("expected commit");
        };
        assert_eq!(c.tree.hex(), "717a800ec7075fc0c5803b2683402dcfcd38fcff");
        assert!(c.parents.is_empty());
        assert_eq!(c.author, "T E <t@e.dev> 1784708656 +0000");
        assert_eq!(c.committer, "T E <t@e.dev> 1784708656 +0000");
        assert_eq!(c.message, "first commit\n");
        assert_eq!(c.subject(), "first commit");
    }

    #[test]
    fn commit_parses_parent_edge_and_multiline_message() {
        let c = GitCommit::parse(CHILD_COMMIT_BODY).unwrap();
        assert_eq!(c.parents.len(), 1);
        assert_eq!(
            c.parents[0].hex(),
            "57d3fe268dca377f8592f29a3fcd9cae7b23e49d"
        );
        assert_eq!(c.message, "second commit\n\nbody line\n");
        assert_eq!(c.subject(), "second commit");
    }

    #[test]
    fn commit_reencodes_byte_identically() {
        // Byte-identity through the object codec: encode(decode(bytes)) == bytes.
        let object = GitObject::Commit(GitCommit::parse(ROOT_COMMIT_BODY).unwrap());
        let encoded = encode(&object);
        assert_eq!(encode(&decode(&encoded).unwrap()), encoded);
        // And the body inside the object is preserved verbatim.
        let GitObject::Commit(c) = &object else {
            panic!("expected commit");
        };
        assert_eq!(c.body(), ROOT_COMMIT_BODY);
    }

    #[test]
    fn commit_with_gpgsig_continuation_reencodes_identically() {
        // A signed commit carries a multi-line `gpgsig` header whose continuation
        // lines begin with a space. The header/message split must not mistake a
        // continuation for the message boundary, and the raw body must round-trip.
        let body: &[u8] = b"tree 717a800ec7075fc0c5803b2683402dcfcd38fcff\n\
            author T E <t@e.dev> 1784708656 +0000\n\
            committer T E <t@e.dev> 1784708656 +0000\n\
            gpgsig -----BEGIN SSH SIGNATURE-----\n \
            U1NIU0lHAAAAAQ==\n \
            -----END SSH SIGNATURE-----\n\
            \n\
            signed commit\n";
        let c = GitCommit::parse(body).unwrap();
        assert_eq!(c.tree.hex(), "717a800ec7075fc0c5803b2683402dcfcd38fcff");
        assert_eq!(c.message, "signed commit\n");
        // The gpgsig block is not modelled but round-trips via the raw body.
        assert_eq!(c.body(), body);
        let object = GitObject::Commit(c);
        assert_eq!(encode(&decode(&encode(&object)).unwrap()), encode(&object));
    }

    #[test]
    fn tag_parses_and_reencodes_byte_identically() {
        let body: &[u8] = b"object 57d3fe268dca377f8592f29a3fcd9cae7b23e49d\n\
            type commit\n\
            tag v1\n\
            tagger T E <t@e.dev> 1784708656 +0000\n\
            \n\
            release one\n";
        let t = GitTag::parse(body).unwrap();
        assert_eq!(t.object.hex(), "57d3fe268dca377f8592f29a3fcd9cae7b23e49d");
        assert_eq!(t.kind, "commit");
        assert_eq!(t.tag, "v1");
        assert_eq!(t.tagger, "T E <t@e.dev> 1784708656 +0000");
        assert_eq!(t.message, "release one\n");
        assert_eq!(t.body(), body);
        let object = GitObject::Tag(t);
        assert_eq!(encode(&decode(&encode(&object)).unwrap()), encode(&object));
    }

    #[test]
    fn commit_missing_tree_is_rejected() {
        let body: &[u8] = b"author X <x@x> 0 +0000\n\ncommitter-less\n";
        assert!(GitCommit::parse(body).is_err());
    }
}
