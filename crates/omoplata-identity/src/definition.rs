//! The **definition graph** — design doc §5.5, principle **P6** (definition
//! identity).
//!
//! Definitions (functions, types, modules, …) are extracted from Rust source
//! per §5.5 using the tree-sitter Rust grammar, receive a stable id at first
//! appearance, and have that identity propagated across versions by a **tiered
//! matcher** whose admission criteria are *categorical rather than
//! scalar-tuned* (§5.5). A definition is thereby "a durable node with its own
//! history, independent of file and line" (P6).
//!
//! Because identity links are bi-temporal *assertions*, mis-matches are
//! correctable, not permanent (§5.5): [`DefinitionGraph::sever`] splits a
//! wrongly-joined identity and [`DefinitionGraph::join`] merges a
//! wrongly-fragmented one.

use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use tree_sitter::Node;
use tree_sitter::Parser;

use crate::error::IdentityError;

/// The kind of a Rust definition, mapped from tree-sitter node kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefinitionKind {
    /// `function_item` — `fn foo() { … }` (including trait methods / impl methods).
    Function,
    /// `struct_item` — `struct Foo { … }`.
    Struct,
    /// `enum_item` — `enum Foo { … }`.
    Enum,
    /// `trait_item` — `trait Foo { … }`.
    Trait,
    /// `impl_item` — `impl Foo { … }` / `impl Trait for Foo { … }`.
    Impl,
    /// `mod_item` — `mod foo { … }`.
    Module,
    /// `type_item` — `type Foo = …;`.
    TypeAlias,
    /// `const_item` — `const FOO: T = …;`.
    Const,
    /// `static_item` — `static FOO: T = …;`.
    Static,
}

impl DefinitionKind {
    /// Map a tree-sitter node kind to a [`DefinitionKind`], if it names a
    /// definition we track.
    fn from_node_kind(kind: &str) -> Option<Self> {
        Some(match kind {
            "function_item" => Self::Function,
            "struct_item" => Self::Struct,
            "enum_item" => Self::Enum,
            "trait_item" => Self::Trait,
            "impl_item" => Self::Impl,
            "mod_item" => Self::Module,
            "type_item" => Self::TypeAlias,
            "const_item" => Self::Const,
            "static_item" => Self::Static,
            _ => return None,
        })
    }

    /// Whether this kind can enclose nested definitions we should descend into.
    fn is_container(self) -> bool {
        matches!(
            self,
            Self::Module | Self::Impl | Self::Trait | Self::Function
        )
    }

    /// A short, stable label used in reports (e.g. `fn`, `struct`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Module => "mod",
            Self::TypeAlias => "type",
            Self::Const => "const",
            Self::Static => "static",
        }
    }
}

/// A single extracted Rust definition.
///
/// `path` is the qualified name (e.g. `outer::inner` or `Foo::method`) built
/// from the enclosing module / impl / function scope; `name` is the leaf.
///
/// `body_hash` is the SHA-256 (hex) of the definition's source text with its
/// leaf name elided — the *normalized-body* hash of §5.5 Tier B. Eliding the
/// name is what makes a pure rename preserve the hash (so it is detectable as a
/// [`Renamed`](MatchStatus::Renamed) rather than an unrelated add/delete), while
/// any edit to the body still changes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The kind of definition.
    pub kind: DefinitionKind,
    /// The qualified name (enclosing scope joined by `::`).
    pub path: String,
    /// The leaf name of the definition.
    pub name: String,
    /// The byte range of the definition within its source text.
    pub byte_range: std::ops::Range<usize>,
    /// SHA-256 (hex) of the definition's source text.
    pub body_hash: String,
}

/// Hash a slice of source text to a lowercase hex SHA-256 digest.
fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Writing hex into a String never fails; avoid unwrap by pushing chars.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// The text a node spans in `source`, or `""` if the range is out of bounds.
fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}

/// The text of a named child field of `node` (e.g. its `name`), if present.
fn field_text(node: Node, source: &str, field: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|child| node_text(child, source).to_owned())
}

/// The *normalized-body* hash of a definition node (§5.5 Tier B): SHA-256 of the
/// node's source text with its leaf `name` identifier elided, so a pure rename
/// preserves the hash while any body edit changes it. Nodes without a `name`
/// field (e.g. `impl`) are hashed in full.
fn body_hash(node: Node, source: &str) -> String {
    let text = node_text(node, source);
    match node.child_by_field_name("name") {
        Some(name) => {
            let node_start = node.start_byte();
            // Byte offsets of the name within the node's own text.
            let lo = name.start_byte().saturating_sub(node_start);
            let hi = name.end_byte().saturating_sub(node_start);
            if lo <= hi
                && hi <= text.len()
                && text.is_char_boundary(lo)
                && text.is_char_boundary(hi)
            {
                let mut normalized = String::with_capacity(text.len());
                normalized.push_str(&text[..lo]);
                normalized.push('\u{0}'); // name placeholder
                normalized.push_str(&text[hi..]);
                sha256_hex(&normalized)
            } else {
                sha256_hex(text)
            }
        }
        None => sha256_hex(text),
    }
}

/// Extract the leaf name for a definition node of a given kind.
fn definition_name(node: Node, source: &str, kind: DefinitionKind) -> String {
    match kind {
        // An impl has no `name` field; name it by the type it is implemented
        // for (the `type` field), e.g. `impl Foo` => `Foo`.
        DefinitionKind::Impl => {
            field_text(node, source, "type").unwrap_or_else(|| "<impl>".to_owned())
        }
        _ => field_text(node, source, "name").unwrap_or_else(|| "<anon>".to_owned()),
    }
}

/// Extract every Rust definition from `source`, in source order.
///
/// Parses with the tree-sitter Rust grammar and walks the tree, descending into
/// modules, impls, traits, and functions to collect nested definitions.
/// Qualified names are built from the enclosing scope. The result is
/// deterministic: a parent definition is emitted before its children, and
/// siblings appear in source order.
///
/// # Errors
///
/// Returns [`IdentityError::Grammar`] if the grammar cannot be loaded, or
/// [`IdentityError::Parse`] if the source cannot be parsed.
pub fn extract_definitions(source: &str) -> Result<Vec<Definition>, IdentityError> {
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|e| IdentityError::Grammar(e.to_string()))?;
    let tree = parser.parse(source, None).ok_or(IdentityError::Parse)?;

    let mut out = Vec::new();
    collect(tree.root_node(), source, "", &mut out);
    Ok(out)
}

/// Whether `source` parses as Rust with no error nodes.
///
/// tree-sitter is deliberately error-tolerant: it recovers from malformed input
/// and still returns a tree (with `ERROR` / `MISSING` nodes), so
/// [`extract_definitions`] succeeds even on invalid source. Callers that must
/// not operate on a best-effort parse — notably the Tier-2 structural merge
/// driver, which would otherwise merge partially-parsed trees — use this to
/// detect malformed input and degrade to a safer path.
///
/// # Errors
///
/// Returns [`IdentityError::Grammar`] if the grammar cannot be loaded, or
/// [`IdentityError::Parse`] if the source yields no tree at all.
pub fn parses_cleanly(source: &str) -> Result<bool, IdentityError> {
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|e| IdentityError::Grammar(e.to_string()))?;
    let tree = parser.parse(source, None).ok_or(IdentityError::Parse)?;
    Ok(!tree.root_node().has_error())
}

/// Recursively collect definitions under `node`, qualifying names with `prefix`.
fn collect(node: Node, source: &str, prefix: &str, out: &mut Vec<Definition>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match DefinitionKind::from_node_kind(child.kind()) {
            Some(kind) => {
                let name = definition_name(child, source, kind);
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}::{name}")
                };
                out.push(Definition {
                    kind,
                    path: path.clone(),
                    name,
                    byte_range: child.byte_range(),
                    body_hash: body_hash(child, source),
                });
                // Descend into containers to find nested definitions, extending
                // the qualified-name prefix with this definition's path.
                if kind.is_container() {
                    collect(child, source, &path, out);
                }
            }
            // Not itself a definition (e.g. a `declaration_list` or an
            // identifier): recurse without extending the prefix so its
            // definition descendants are still found.
            None => collect(child, source, prefix, out),
        }
    }
}

/// The classification of a matched / unmatched definition across two versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchStatus {
    /// Present only in the new version; mints a fresh identity.
    Added,
    /// Present only in the old version; the definition was removed.
    Deleted,
    /// Same identity, byte-identical body.
    Unchanged,
    /// Same identity (same `(kind, path)`), body edited.
    Modified,
    /// Same identity carried across a rename or move (path changed).
    Renamed,
}

/// One entry in a [`match_definitions`] report.
///
/// `old` / `new` are indices into the respective input slices (so a caller can
/// recover full [`Definition`]s), and `reason` records which matcher tier
/// produced the classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefMatch {
    /// Index into the `old` slice, if this entry corresponds to an old def.
    pub old: Option<usize>,
    /// Index into the `new` slice, if this entry corresponds to a new def.
    pub new: Option<usize>,
    /// How the definition was classified.
    pub status: MatchStatus,
    /// The matcher tier / rule that produced this classification.
    pub reason: &'static str,
}

/// The **tiered matcher** of §5.5: categorical admission, not a tuned scalar.
///
/// Identity is propagated old → new by three deterministic tiers, applied in
/// order; whatever remains is classified as added / deleted:
///
/// * **Tier 1 — exact.** Same `(kind, path)` ⇒ same definition. If the body
///   hash also matches it is [`Unchanged`](MatchStatus::Unchanged); otherwise
///   [`Modified`](MatchStatus::Modified).
/// * **Tier 2 — rename.** An unmatched old and new with the same
///   `(kind, body_hash)` but a different path ⇒ the same definition, renamed
///   ([`Renamed`](MatchStatus::Renamed)).
/// * **Tier 3 — move.** An unmatched old and new with the same
///   `(kind, name, body_hash)` across different paths ⇒ a move
///   ([`Renamed`](MatchStatus::Renamed), reason distinguishes it).
/// * Remaining old ⇒ [`Deleted`](MatchStatus::Deleted); remaining new ⇒
///   [`Added`](MatchStatus::Added) (which is where a fresh id would be minted).
///
/// This mirrors the design's honesty asymmetry: the tiers only assert identity
/// on exact `(kind, path)` or exact body-hash evidence — when uncertain it
/// prefers to mint new rather than guess a hard match (§5.5).
///
/// The report order is deterministic: matched pairs in new-index order, then
/// deletions in old-index order, then additions in new-index order.
pub fn match_definitions(old: &[Definition], new: &[Definition]) -> Vec<DefMatch> {
    let mut matched_old = vec![false; old.len()];
    let mut matched_new = vec![false; new.len()];
    let mut matches: Vec<DefMatch> = Vec::new();

    // Tier 1 — exact (kind, path).
    for (j, nd) in new.iter().enumerate() {
        if let Some(i) = old
            .iter()
            .enumerate()
            .position(|(i, od)| !matched_old[i] && od.kind == nd.kind && od.path == nd.path)
        {
            matched_old[i] = true;
            matched_new[j] = true;
            let (status, reason) = if old[i].body_hash == nd.body_hash {
                (
                    MatchStatus::Unchanged,
                    "tier1: exact (kind,path), body identical",
                )
            } else {
                (
                    MatchStatus::Modified,
                    "tier1: exact (kind,path), body edited",
                )
            };
            matches.push(DefMatch {
                old: Some(i),
                new: Some(j),
                status,
                reason,
            });
        }
    }

    // Tier 2 — rename: same (kind, body_hash), different path.
    for (j, nd) in new.iter().enumerate() {
        if matched_new[j] {
            continue;
        }
        if let Some(i) = old.iter().enumerate().position(|(i, od)| {
            !matched_old[i]
                && od.kind == nd.kind
                && od.body_hash == nd.body_hash
                && od.path != nd.path
        }) {
            matched_old[i] = true;
            matched_new[j] = true;
            matches.push(DefMatch {
                old: Some(i),
                new: Some(j),
                status: MatchStatus::Renamed,
                reason: "tier2: identical body, path changed (rename)",
            });
        }
    }

    // Tier 3 — move: same (kind, name, body_hash), different path.
    for (j, nd) in new.iter().enumerate() {
        if matched_new[j] {
            continue;
        }
        if let Some(i) = old.iter().enumerate().position(|(i, od)| {
            !matched_old[i]
                && od.kind == nd.kind
                && od.name == nd.name
                && od.body_hash == nd.body_hash
                && od.path != nd.path
        }) {
            matched_old[i] = true;
            matched_new[j] = true;
            matches.push(DefMatch {
                old: Some(i),
                new: Some(j),
                status: MatchStatus::Renamed,
                reason: "tier3: same name and body, container moved",
            });
        }
    }

    // Remaining old => Deleted (old-index order).
    for (i, _) in old.iter().enumerate() {
        if !matched_old[i] {
            matches.push(DefMatch {
                old: Some(i),
                new: None,
                status: MatchStatus::Deleted,
                reason: "no surviving match in new version",
            });
        }
    }

    // Remaining new => Added (new-index order); this is where a fresh id is minted.
    for (j, _) in new.iter().enumerate() {
        if !matched_new[j] {
            matches.push(DefMatch {
                old: None,
                new: Some(j),
                status: MatchStatus::Added,
                reason: "first appearance: mint new identity",
            });
        }
    }

    matches
}

/// A stable definition identity, minted at first appearance (§5.5, P6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefinitionId(String);

impl DefinitionId {
    /// Borrow the underlying identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DefinitionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An occurrence of a definition observed in some version (a graph node id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OccurrenceId(u64);

impl OccurrenceId {
    /// The raw numeric id.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The **definition graph**: occurrences of definitions and the *assertions*
/// that resolve them to stable identities (§5.5).
///
/// Each observed definition is an [`OccurrenceId`]; a resolved
/// [`DefinitionId`] groups occurrences believed to be the same durable node.
/// Because identity links are bi-temporal assertions, they are correctable:
/// [`join`](Self::join) merges two identities (heals wrong fragmentation) and
/// [`sever`](Self::sever) splits one off (undoes a wrong merge). Both mutate the
/// *resolved-now* identity while leaving the occurrence history intact.
#[derive(Debug, Default)]
pub struct DefinitionGraph {
    next_occ: u64,
    next_id: u64,
    /// The definition text/metadata for each occurrence.
    occurrences: HashMap<OccurrenceId, Definition>,
    /// The currently-resolved identity of each occurrence.
    identity: HashMap<OccurrenceId, DefinitionId>,
}

impl DefinitionGraph {
    /// Create an empty definition graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint the next fresh definition identity.
    fn mint_id(&mut self) -> DefinitionId {
        self.next_id += 1;
        DefinitionId(format!("def-{}", self.next_id))
    }

    /// Record a newly observed definition, minting a fresh identity for it
    /// (first appearance ⇒ new id, §5.5).
    pub fn add(&mut self, def: Definition) -> OccurrenceId {
        let occ = OccurrenceId(self.next_occ);
        self.next_occ += 1;
        let id = self.mint_id();
        self.occurrences.insert(occ, def);
        self.identity.insert(occ, id);
        occ
    }

    /// The definition metadata for an occurrence.
    pub fn definition(&self, occ: OccurrenceId) -> Option<&Definition> {
        self.occurrences.get(&occ)
    }

    /// The currently-resolved identity of an occurrence.
    pub fn identity_of(&self, occ: OccurrenceId) -> Option<&DefinitionId> {
        self.identity.get(&occ)
    }

    /// Every occurrence currently resolved to the given identity, sorted.
    pub fn members(&self, id: &DefinitionId) -> Vec<OccurrenceId> {
        let mut v: Vec<OccurrenceId> = self
            .identity
            .iter()
            .filter(|(_, did)| *did == id)
            .map(|(occ, _)| *occ)
            .collect();
        v.sort();
        v
    }

    /// **join** (§5.5): assert that two occurrences are the same durable
    /// definition, healing a wrong fragmentation. Every occurrence sharing
    /// `b`'s identity is relabelled to `a`'s identity.
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownOccurrence`] if either occurrence is unknown.
    pub fn join(&mut self, a: OccurrenceId, b: OccurrenceId) -> Result<(), IdentityError> {
        let target = self
            .identity
            .get(&a)
            .ok_or(IdentityError::UnknownOccurrence(a.0))?
            .clone();
        let source = self
            .identity
            .get(&b)
            .ok_or(IdentityError::UnknownOccurrence(b.0))?
            .clone();
        if source == target {
            return Ok(());
        }
        for did in self.identity.values_mut() {
            if *did == source {
                *did = target.clone();
            }
        }
        Ok(())
    }

    /// **sever** (§5.5): split an occurrence off its current identity into a
    /// fresh one, undoing a wrong merge. Other occurrences keep the old id.
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownOccurrence`] if the occurrence is unknown.
    pub fn sever(&mut self, occ: OccurrenceId) -> Result<DefinitionId, IdentityError> {
        if !self.identity.contains_key(&occ) {
            return Err(IdentityError::UnknownOccurrence(occ.0));
        }
        let fresh = self.mint_id();
        self.identity.insert(occ, fresh.clone());
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_kinds_names_and_order() {
        let src = r#"
use std::fmt;

const K: u32 = 1;

struct Point { x: i32, y: i32 }

enum Shape { Circle, Square }

fn free() -> u32 { K }

mod inner {
    fn nested() {}
    type Alias = u32;
}

impl Point {
    fn origin() -> Point { Point { x: 0, y: 0 } }
}
"#;
        let defs = extract_definitions(src).unwrap();
        let got: Vec<(DefinitionKind, &str)> =
            defs.iter().map(|d| (d.kind, d.path.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (DefinitionKind::Const, "K"),
                (DefinitionKind::Struct, "Point"),
                (DefinitionKind::Enum, "Shape"),
                (DefinitionKind::Function, "free"),
                (DefinitionKind::Module, "inner"),
                (DefinitionKind::Function, "inner::nested"),
                (DefinitionKind::TypeAlias, "inner::Alias"),
                (DefinitionKind::Impl, "Point"),
                (DefinitionKind::Function, "Point::origin"),
            ]
        );
    }

    #[test]
    fn unchanged_modified_added_deleted() {
        let old_src = "fn a() { let x = 1; }\nfn gone() {}\n";
        let new_src = "fn a() { let x = 1; }\nfn b() { let y = 2; }\n";
        let old = extract_definitions(old_src).unwrap();
        let new = extract_definitions(new_src).unwrap();
        let report = match_definitions(&old, &new);

        // a: unchanged; gone: deleted; b: added.
        let a = report.iter().find(|m| m.new == Some(0)).unwrap();
        assert_eq!(a.status, MatchStatus::Unchanged);
        assert!(report
            .iter()
            .any(|m| m.status == MatchStatus::Deleted && old[m.old.unwrap()].name == "gone"));
        assert!(report
            .iter()
            .any(|m| m.status == MatchStatus::Added && new[m.new.unwrap()].name == "b"));
    }

    #[test]
    fn body_edit_is_modified() {
        let old = extract_definitions("fn a() { 1 }\n").unwrap();
        let new = extract_definitions("fn a() { 2 }\n").unwrap();
        let report = match_definitions(&old, &new);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].status, MatchStatus::Modified);
    }

    #[test]
    fn identical_body_new_path_is_renamed() {
        let old = extract_definitions("fn foo() { let x = 41 + 1; }\n").unwrap();
        let new = extract_definitions("fn bar() { let x = 41 + 1; }\n").unwrap();
        let report = match_definitions(&old, &new);
        let renamed = report
            .iter()
            .find(|m| m.status == MatchStatus::Renamed)
            .expect("expected a rename");
        assert_eq!(old[renamed.old.unwrap()].name, "foo");
        assert_eq!(new[renamed.new.unwrap()].name, "bar");
    }

    #[test]
    fn sever_and_join_change_resolved_identity() {
        let mut g = DefinitionGraph::new();
        let def = |name: &str| Definition {
            kind: DefinitionKind::Function,
            path: name.to_owned(),
            name: name.to_owned(),
            byte_range: 0..0,
            body_hash: "h".to_owned(),
        };
        let a = g.add(def("a"));
        let b = g.add(def("b"));
        // Initially distinct identities.
        assert_ne!(g.identity_of(a), g.identity_of(b));

        // join: b now shares a's identity (heals fragmentation).
        g.join(a, b).unwrap();
        assert_eq!(g.identity_of(a), g.identity_of(b));

        // sever: b splits off again (undoes the merge).
        g.sever(b).unwrap();
        assert_ne!(g.identity_of(a), g.identity_of(b));
    }
}
