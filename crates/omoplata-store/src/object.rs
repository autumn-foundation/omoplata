//! Content-addressed object model for omoplata.
//!
//! Objects are addressed by a hash-agile [`ObjectId`] (algorithm tag + digest)
//! computed over a canonical, self-describing serialization. v1 uses SHA-256.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

/// Hash algorithm used to address an object. Hash-agile: the algorithm is
/// carried alongside the digest so the address format is not tied to SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlg {
    /// SHA-256 — the only algorithm in v1.
    Sha256,
}

impl HashAlg {
    /// Stable lowercase tag used in ids and on-disk paths.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HashAlg::Sha256 => "sha256",
        }
    }

    fn digest_len(self) -> usize {
        match self {
            HashAlg::Sha256 => 32,
        }
    }
}

impl fmt::Display for HashAlg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HashAlg {
    type Err = ObjectError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sha256" => Ok(HashAlg::Sha256),
            other => Err(ObjectError::UnknownHashAlg(other.to_owned())),
        }
    }
}

/// A content address: a hash algorithm tag plus the raw digest bytes.
///
/// Text form is `"<alg>:<hex>"`, e.g. `sha256:e3b0c4...`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectId {
    alg: HashAlg,
    digest: Vec<u8>,
}

impl ObjectId {
    /// The hash algorithm this id uses.
    #[must_use]
    pub fn alg(&self) -> HashAlg {
        self.alg
    }

    /// The raw digest bytes.
    #[must_use]
    pub fn digest(&self) -> &[u8] {
        &self.digest
    }

    /// Lowercase hex of the digest (no algorithm prefix).
    #[must_use]
    pub fn hex(&self) -> String {
        hex_encode(&self.digest)
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.alg, self.hex())
    }
}

impl FromStr for ObjectId {
    type Err = ObjectError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (alg, hex) = s
            .split_once(':')
            .ok_or_else(|| ObjectError::MalformedId(s.to_owned()))?;
        let alg: HashAlg = alg.parse()?;
        let digest = hex_decode(hex).ok_or_else(|| ObjectError::MalformedId(s.to_owned()))?;
        if digest.len() != alg.digest_len() {
            return Err(ObjectError::MalformedId(s.to_owned()));
        }
        Ok(ObjectId { alg, digest })
    }
}

/// The kind tag stored in an object envelope header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    /// Opaque byte content.
    Blob,
    /// A directory-like mapping of names to object ids.
    Tree,
}

impl ObjectKind {
    fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "blob" => Some(ObjectKind::Blob),
            "tree" => Some(ObjectKind::Tree),
            _ => None,
        }
    }
}

/// Opaque byte content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob(Vec<u8>);

impl Blob {
    /// Wrap raw bytes as a blob.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Blob(bytes.into())
    }

    /// The blob's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Whether a tree entry points at a blob or a subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Points at a [`Blob`].
    Blob,
    /// Points at a [`Tree`].
    Tree,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            EntryKind::Blob => "blob",
            EntryKind::Tree => "tree",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "blob" => Some(EntryKind::Blob),
            "tree" => Some(EntryKind::Tree),
            _ => None,
        }
    }
}

/// A single named entry in a [`Tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Entry name (a single path component).
    pub name: String,
    /// Whether the entry is a blob or a subtree.
    pub kind: EntryKind,
    /// Content address of the referenced object.
    pub id: ObjectId,
}

/// A directory-like object: name -> (kind, id), kept sorted by name so that
/// serialization is canonical and the object id is order-independent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

impl Tree {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Tree {
            entries: Vec::new(),
        }
    }

    /// Insert or replace an entry, keeping entries sorted and unique by name.
    ///
    /// # Errors
    /// Returns [`ObjectError::InvalidName`] if `name` is empty, `.`/`..`, or
    /// contains `/`, NUL, or newline.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        kind: EntryKind,
        id: ObjectId,
    ) -> Result<(), ObjectError> {
        let name = name.into();
        validate_name(&name)?;
        let entry = TreeEntry { name, kind, id };
        match self
            .entries
            .binary_search_by(|e| e.name.as_str().cmp(entry.name.as_str()))
        {
            Ok(i) => self.entries[i] = entry,
            Err(i) => self.entries.insert(i, entry),
        }
        Ok(())
    }

    /// The entries, sorted by name.
    #[must_use]
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// Look up an entry by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i])
    }
}

fn validate_name(name: &str) -> Result<(), ObjectError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\0')
        || name.contains('\n')
    {
        return Err(ObjectError::InvalidName(name.to_owned()));
    }
    Ok(())
}

/// A parsed omoplata object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Object {
    /// Opaque bytes.
    Blob(Blob),
    /// A tree of named entries.
    Tree(Tree),
}

impl Object {
    /// This object's kind.
    #[must_use]
    pub fn kind(&self) -> ObjectKind {
        match self {
            Object::Blob(_) => ObjectKind::Blob,
            Object::Tree(_) => ObjectKind::Tree,
        }
    }

    /// Serialize to the canonical byte form `"{kind} {payload_len}\0{payload}"`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let payload = self.payload();
        let header = format!("{} {}", self.kind().as_str(), payload.len());
        let mut out = Vec::with_capacity(header.len() + 1 + payload.len());
        out.extend_from_slice(header.as_bytes());
        out.push(0);
        out.extend_from_slice(&payload);
        out
    }

    fn payload(&self) -> Vec<u8> {
        match self {
            Object::Blob(b) => b.0.clone(),
            Object::Tree(t) => {
                let mut out = Vec::new();
                for e in &t.entries {
                    // "{kind} {id} {name}\n" — name last so it may contain spaces.
                    out.extend_from_slice(e.kind.as_str().as_bytes());
                    out.push(b' ');
                    out.extend_from_slice(e.id.to_string().as_bytes());
                    out.push(b' ');
                    out.extend_from_slice(e.name.as_bytes());
                    out.push(b'\n');
                }
                out
            }
        }
    }

    /// Parse an object from its canonical byte form.
    ///
    /// # Errors
    /// Returns [`ObjectError::Corrupt`] if the header, length, or body is
    /// malformed.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, ObjectError> {
        let nul = bytes
            .iter()
            .position(|&b| b == 0)
            .ok_or(ObjectError::Corrupt("missing header terminator"))?;
        let header = std::str::from_utf8(&bytes[..nul])
            .map_err(|_| ObjectError::Corrupt("non-utf8 header"))?;
        let (kind_s, len_s) = header
            .split_once(' ')
            .ok_or(ObjectError::Corrupt("malformed header"))?;
        let kind = ObjectKind::parse(kind_s).ok_or(ObjectError::Corrupt("unknown kind"))?;
        let len: usize = len_s
            .parse()
            .map_err(|_| ObjectError::Corrupt("bad length"))?;
        let payload = &bytes[nul + 1..];
        if payload.len() != len {
            return Err(ObjectError::Corrupt("length mismatch"));
        }
        match kind {
            ObjectKind::Blob => Ok(Object::Blob(Blob(payload.to_vec()))),
            ObjectKind::Tree => Ok(Object::Tree(parse_tree(payload)?)),
        }
    }

    /// Content address under SHA-256.
    #[must_use]
    pub fn id(&self) -> ObjectId {
        self.id_with(HashAlg::Sha256)
    }

    /// Content address under `alg`.
    #[must_use]
    pub fn id_with(&self, alg: HashAlg) -> ObjectId {
        let bytes = self.serialize();
        let digest = match alg {
            HashAlg::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                hasher.finalize().to_vec()
            }
        };
        ObjectId { alg, digest }
    }
}

fn parse_tree(payload: &[u8]) -> Result<Tree, ObjectError> {
    let mut tree = Tree::new();
    if payload.is_empty() {
        return Ok(tree);
    }
    let text =
        std::str::from_utf8(payload).map_err(|_| ObjectError::Corrupt("non-utf8 tree payload"))?;
    for line in text.split_terminator('\n') {
        let mut parts = line.splitn(3, ' ');
        let kind_s = parts
            .next()
            .ok_or(ObjectError::Corrupt("tree entry: no kind"))?;
        let id_s = parts
            .next()
            .ok_or(ObjectError::Corrupt("tree entry: no id"))?;
        let name = parts
            .next()
            .ok_or(ObjectError::Corrupt("tree entry: no name"))?;
        let kind = EntryKind::parse(kind_s).ok_or(ObjectError::Corrupt("tree entry: bad kind"))?;
        let id: ObjectId = id_s
            .parse()
            .map_err(|_| ObjectError::Corrupt("tree entry: bad id"))?;
        tree.insert(name, kind, id)
            .map_err(|_| ObjectError::Corrupt("tree entry: bad name"))?;
    }
    Ok(tree)
}

/// Errors from the object model.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ObjectError {
    /// Hash algorithm tag not recognized.
    #[error("unknown hash algorithm: {0}")]
    UnknownHashAlg(String),
    /// Object id string could not be parsed.
    #[error("malformed object id: {0}")]
    MalformedId(String),
    /// Tree entry name is not a valid single path component.
    #[error("invalid tree entry name: {0:?}")]
    InvalidName(String),
    /// Serialized bytes are not a well-formed object.
    #[error("corrupt object: {0}")]
    Corrupt(&'static str),
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
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id(seed: u8) -> ObjectId {
        Object::Blob(Blob::new(vec![seed])).id()
    }

    #[test]
    fn blob_roundtrip() {
        let obj = Object::Blob(Blob::new(b"abc".to_vec()));
        let bytes = obj.serialize();
        assert_eq!(Object::deserialize(&bytes).unwrap(), obj);
    }

    #[test]
    fn blob_id_is_deterministic() {
        let a = Object::Blob(Blob::new(b"same".to_vec())).id();
        let b = Object::Blob(Blob::new(b"same".to_vec())).id();
        assert_eq!(a, b);
        assert_ne!(a, Object::Blob(Blob::new(b"diff".to_vec())).id());
        assert!(a.to_string().starts_with("sha256:"));
    }

    #[test]
    fn object_id_display_parse_roundtrip() {
        let id = sample_id(7);
        let parsed: ObjectId = id.to_string().parse().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn object_id_rejects_garbage() {
        assert!("nope".parse::<ObjectId>().is_err());
        assert!("sha256:zz".parse::<ObjectId>().is_err());
        assert!("sha256:00".parse::<ObjectId>().is_err()); // wrong length
        assert!("md5:00".parse::<ObjectId>().is_err());
    }

    #[test]
    fn tree_id_is_order_independent() {
        let mut t1 = Tree::new();
        t1.insert("b", EntryKind::Blob, sample_id(1)).unwrap();
        t1.insert("a", EntryKind::Blob, sample_id(2)).unwrap();
        let mut t2 = Tree::new();
        t2.insert("a", EntryKind::Blob, sample_id(2)).unwrap();
        t2.insert("b", EntryKind::Blob, sample_id(1)).unwrap();
        assert_eq!(Object::Tree(t1).id(), Object::Tree(t2).id());
    }

    #[test]
    fn tree_roundtrip_and_lookup() {
        let mut t = Tree::new();
        t.insert("src", EntryKind::Tree, sample_id(3)).unwrap();
        t.insert("README.md", EntryKind::Blob, sample_id(4))
            .unwrap();
        let obj = Object::Tree(t);
        let back = Object::deserialize(&obj.serialize()).unwrap();
        assert_eq!(back, obj);
        if let Object::Tree(t) = back {
            assert_eq!(t.entries()[0].name, "README.md"); // sorted first
            assert_eq!(t.get("src").unwrap().kind, EntryKind::Tree);
        } else {
            panic!("expected tree");
        }
    }

    #[test]
    fn invalid_names_rejected() {
        let mut t = Tree::new();
        assert!(t.insert("", EntryKind::Blob, sample_id(1)).is_err());
        assert!(t.insert("a/b", EntryKind::Blob, sample_id(1)).is_err());
        assert!(t.insert("..", EntryKind::Blob, sample_id(1)).is_err());
    }

    #[test]
    fn deserialize_rejects_malformed() {
        assert!(Object::deserialize(b"blob 99\0hi").is_err()); // length mismatch
        assert!(Object::deserialize(b"nul-free").is_err()); // no terminator
    }
}
