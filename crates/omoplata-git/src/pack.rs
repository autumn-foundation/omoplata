//! Packfile and delta decoding — reading `git gc`'d repositories.
//!
//! After `git gc` (or `git repack`), most objects no longer live as loose
//! `<xx>/<38 hex>` files: they are consolidated into a *packfile*
//! (`objects/pack/pack-<hash>.pack`) with a companion *pack index*
//! (`pack-<hash>.idx`). Objects inside a packfile are stored either whole
//! (zlib-deflated) or as *deltas* against another object in the pack — a
//! compact instruction stream that reconstructs the target from a base. This
//! module decodes both, so a packed repository imports and verifies through the
//! I9 round-trip gate exactly like a loose one.
//!
//! ## What is decoded
//! - **Pack index v2** ([`parse_idx`]): the `\377tOc` magic, the 256-entry
//!   fanout table, the sorted 20-byte object names, their offsets (including the
//!   8-byte large-offset table for packs over 2 GiB), yielding `oid → offset`.
//! - **Packfile v2/v3** ([`read_pack`]): the `PACK` header, each object's
//!   varint `(type, size)` header, whole objects (`commit`/`tree`/`blob`/`tag`),
//!   and both delta encodings — `OFS_DELTA` (base by back-offset) and
//!   `REF_DELTA` (base by oid).
//! - **Delta application** ([`apply_delta`]): the copy/insert instruction stream
//!   that rebuilds a target object's bytes from its base. Delta chains are
//!   resolved recursively with memoization and a depth guard.
//!
//! Every reconstructed object's recomputed SHA-1 is checked against the oid the
//! index claims, so a corrupt pack cannot smuggle in a mislabelled object.
//!
//! No `unwrap`/`expect`/`panic` appears outside tests.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;

use crate::error::GitError;
use crate::object::{decode, sha1_bytes, GitObject, GitOid};

/// The `\377tOc` magic that opens a pack index v2 file.
const IDX_MAGIC: [u8; 4] = [0xff, 0x74, 0x4f, 0x63];
/// The `PACK` magic that opens a packfile.
const PACK_MAGIC: [u8; 4] = *b"PACK";
/// The high bit of a 4-byte idx offset: when set, the low 31 bits index the
/// 8-byte large-offset table rather than being the offset directly.
const IDX_LARGE_OFFSET_FLAG: u32 = 0x8000_0000;
/// Maximum delta-chain / recursion depth before a pack is rejected as
/// pathological (git's own default pack depth is 50; this is a safe ceiling that
/// still rejects cycles introduced by a malformed `REF_DELTA`).
const MAX_DELTA_DEPTH: usize = 200;
/// Upper bound on any single object or delta stream size, guarding against a
/// malformed length triggering an unbounded allocation.
const MAX_OBJECT_SIZE: u64 = 1 << 34;

/// Per-pack decode statistics, including how many delta objects were resolved.
///
/// Callers (and tests) use [`PackDecode::delta_objects`] to confirm a delta code
/// path was actually exercised against a real `git gc`'d repository.
#[derive(Debug, Clone, Default)]
pub struct PackDecode {
    /// Every object in the pack, as `(oid, decoded object)` in index order.
    pub objects: Vec<(GitOid, GitObject)>,
    /// Number of `OFS_DELTA` (offset-based delta) entries encountered.
    pub ofs_deltas: usize,
    /// Number of `REF_DELTA` (oid-based delta) entries encountered.
    pub ref_deltas: usize,
}

impl PackDecode {
    /// Total number of delta-encoded objects (`OFS_DELTA` + `REF_DELTA`).
    #[must_use]
    pub fn delta_objects(&self) -> usize {
        self.ofs_deltas + self.ref_deltas
    }
}

/// Read and fully decode the packfile at `pack_path`, resolving every object
/// (including deltas) to a concrete [`GitObject`].
///
/// The companion index (`<stem>.idx`) is read alongside to obtain the object
/// oids and their offsets. Each reconstructed object's recomputed SHA-1 is
/// checked against the oid the index claims.
///
/// # Errors
/// Returns [`GitError::Pack`] on a malformed index or packfile (bad magic,
/// unsupported version, truncated stream, delta out of range, or an oid that
/// does not match its reconstructed content), [`GitError::Zlib`] on a corrupt
/// deflate stream, [`GitError::Decode`] if a reconstructed object is not a
/// well-formed git object, or [`GitError::Io`] if either file cannot be read.
pub fn read_pack(pack_path: &Path) -> Result<Vec<(GitOid, GitObject)>, GitError> {
    read_pack_detailed(pack_path).map(|d| d.objects)
}

/// Like [`read_pack`] but also reports delta statistics ([`PackDecode`]).
///
/// # Errors
/// See [`read_pack`].
pub fn read_pack_detailed(pack_path: &Path) -> Result<PackDecode, GitError> {
    let idx_path = pack_path.with_extension("idx");
    let idx_bytes = std::fs::read(&idx_path).map_err(|source| GitError::Io {
        path: idx_path.clone(),
        source,
    })?;
    let pack_bytes = std::fs::read(pack_path).map_err(|source| GitError::Io {
        path: pack_path.to_path_buf(),
        source,
    })?;

    let index = parse_idx(&idx_bytes)?;
    let pack_count = parse_pack_header(&pack_bytes)?;
    if pack_count as usize != index.len() {
        return Err(GitError::Pack(
            "pack object count does not match index entry count",
        ));
    }

    // oid → offset, for resolving REF_DELTA bases.
    let mut oid_to_offset: HashMap<GitOid, u64> = HashMap::with_capacity(index.len());
    for (oid, offset) in &index {
        oid_to_offset.insert(*oid, *offset);
    }

    let mut memo: HashMap<u64, (u8, Vec<u8>)> = HashMap::new();
    let mut out = PackDecode::default();
    for (oid, offset) in &index {
        // Classify the delta kind (cheap header peek, no inflate).
        match peek_type(&pack_bytes, *offset)? {
            OBJ_OFS_DELTA => out.ofs_deltas += 1,
            OBJ_REF_DELTA => out.ref_deltas += 1,
            _ => {}
        }
        let (type_id, body) = resolve(&pack_bytes, *offset, &oid_to_offset, &mut memo, 0)?;
        let canonical = canonical_bytes(type_id, &body)?;
        let computed = GitOid::from_bytes(sha1_bytes(&canonical));
        if &computed != oid {
            return Err(GitError::Pack(
                "reconstructed packed object does not match its index oid",
            ));
        }
        let object = decode(&canonical)?;
        out.objects.push((*oid, object));
    }
    Ok(out)
}

/// Decode a self-contained packfile held **entirely in memory** — the form a
/// packfile arrives in over the git wire protocol (`upload-pack`), where there
/// is no companion `*.idx` and the stream ends with a 20-byte SHA-1 trailer
/// rather than index-supplied oids.
///
/// Unlike [`read_pack`] (which reads object oids and offsets from a sidecar
/// index), this walks the pack sequentially: it discovers each object's offset
/// by inflating in order, resolves `OFS_DELTA` bases by back-offset and
/// `REF_DELTA` bases by oid (building the oid→offset map incrementally, with
/// deferral passes so a ref-delta whose base appears later still resolves), and
/// recomputes each object's SHA-1 from its reconstructed content. A full clone's
/// pack is self-contained (not thin), so every delta base resolves within it.
///
/// The 20-byte trailing pack checksum is tolerated (it is simply not part of any
/// object and the sequential walk stops after the declared object count).
///
/// # Errors
/// Returns [`GitError::Pack`] on a malformed header, a truncated stream, a delta
/// that cannot be applied, a `REF_DELTA` whose base is absent from the pack (a
/// *thin* pack, which a full clone never produces), or a reconstructed object
/// whose recomputed content is not a well-formed git object; [`GitError::Zlib`]
/// on a corrupt deflate stream; or [`GitError::Decode`] on a malformed object.
pub fn parse_pack_bytes(pack: &[u8]) -> Result<Vec<(GitOid, GitObject)>, GitError> {
    let count = parse_pack_header(pack)? as usize;

    // Pass 1: discover every object's byte offset by inflating sequentially. The
    // pack has no index, so the only way to find object N+1 is to consume object
    // N's zlib stream and note where it ends.
    let mut offsets: Vec<u64> = Vec::with_capacity(count);
    let mut pos = 12usize; // 4 magic + 4 version + 4 count
    for _ in 0..count {
        offsets.push(pos as u64);
        pos = skip_entry(pack, pos)?;
    }

    // Pass 2: resolve every object, deferring ref-deltas whose base oid is not
    // yet known. OFS-deltas resolve immediately (their base offset is explicit
    // and always earlier). Iterating until no progress bounds the work at
    // O(objects) passes and rejects a thin pack (an unresolvable ref-delta base).
    let mut oid_to_offset: HashMap<GitOid, u64> = HashMap::with_capacity(count);
    let mut memo: HashMap<u64, (u8, Vec<u8>)> = HashMap::new();
    let mut results: HashMap<u64, (GitOid, GitObject)> = HashMap::with_capacity(count);
    let mut remaining: Vec<u64> = offsets.clone();
    while !remaining.is_empty() {
        let before = remaining.len();
        let mut deferred: Vec<u64> = Vec::new();
        for &offset in &remaining {
            // A ref-delta whose base has not been resolved yet is deferred to a
            // later pass rather than resolved eagerly (its base may sit later in
            // the pack).
            if let Some(base_oid) = peek_ref_base(pack, offset)? {
                if !oid_to_offset.contains_key(&base_oid) {
                    deferred.push(offset);
                    continue;
                }
            }
            let (type_id, body) = resolve(pack, offset, &oid_to_offset, &mut memo, 0)?;
            let canonical = canonical_bytes(type_id, &body)?;
            let computed = GitOid::from_bytes(sha1_bytes(&canonical));
            let object = decode(&canonical)?;
            oid_to_offset.insert(computed, offset);
            results.insert(offset, (computed, object));
        }
        if deferred.len() == before {
            return Err(GitError::Pack(
                "packfile: unresolvable ref-delta base (thin pack not supported)",
            ));
        }
        remaining = deferred;
    }

    // Emit in pack order for determinism.
    let mut out = Vec::with_capacity(count);
    for offset in &offsets {
        if let Some(entry) = results.remove(offset) {
            out.push(entry);
        }
    }
    Ok(out)
}

/// Advance past the pack entry at `offset`, returning the offset of the next
/// entry. Consumes the entry header, any delta base reference, and the object's
/// zlib stream (whose compressed length is reported by the inflater).
fn skip_entry(pack: &[u8], offset: usize) -> Result<usize, GitError> {
    let mut pos = offset;
    let (type_id, _size) = read_entry_header(pack, &mut pos)?;
    match type_id {
        OBJ_COMMIT | OBJ_TREE | OBJ_BLOB | OBJ_TAG => {}
        OBJ_OFS_DELTA => {
            let _ = read_ofs_delta_offset(pack, &mut pos)?;
        }
        OBJ_REF_DELTA => {
            pos = pos
                .checked_add(20)
                .ok_or(GitError::Pack("packfile: offset overflow"))?;
            if pos > pack.len() {
                return Err(GitError::Pack("packfile: truncated ref-delta base oid"));
            }
        }
        _ => return Err(GitError::Pack("packfile: unknown object type")),
    }
    let consumed = inflate_consumed(pack, pos)?;
    pos.checked_add(consumed)
        .ok_or(GitError::Pack("packfile: offset overflow"))
}

/// If the entry at `offset` is a `REF_DELTA`, return its base oid (without
/// inflating the delta stream); otherwise `None`.
fn peek_ref_base(pack: &[u8], offset: u64) -> Result<Option<GitOid>, GitError> {
    let mut pos =
        usize::try_from(offset).map_err(|_| GitError::Pack("packfile: offset overflow"))?;
    let (type_id, _size) = read_entry_header(pack, &mut pos)?;
    if type_id != OBJ_REF_DELTA {
        return Ok(None);
    }
    let end = pos
        .checked_add(20)
        .ok_or(GitError::Pack("packfile: offset overflow"))?;
    let oid_bytes = pack
        .get(pos..end)
        .ok_or(GitError::Pack("packfile: truncated ref-delta base oid"))?;
    let mut oid = [0u8; 20];
    oid.copy_from_slice(oid_bytes);
    Ok(Some(GitOid::from_bytes(oid)))
}

/// Enumerate and decode every `*.pack` under `<git_dir>/objects/pack`.
///
/// Returns the concatenation of every pack's objects. A repository with no pack
/// directory (or no packs) yields an empty list.
///
/// # Errors
/// Propagates any [`GitError`] from reading or decoding a packfile.
pub fn read_all_packs(git_dir: &Path) -> Result<Vec<(GitOid, GitObject)>, GitError> {
    let mut out = Vec::new();
    for pack in pack_paths(git_dir)? {
        out.extend(read_pack(&pack)?);
    }
    Ok(out)
}

/// The paths of every `*.pack` file under `<git_dir>/objects/pack`, sorted for
/// determinism.
///
/// # Errors
/// Returns [`GitError::Io`] if the pack directory exists but cannot be listed.
pub fn pack_paths(git_dir: &Path) -> Result<Vec<PathBuf>, GitError> {
    let pack_dir = git_dir.join("objects").join("pack");
    let rd = match std::fs::read_dir(&pack_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(GitError::Io {
                path: pack_dir,
                source,
            })
        }
    };
    let mut packs = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|source| GitError::Io {
            path: pack_dir.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "pack") {
            packs.push(path);
        }
    }
    packs.sort();
    Ok(packs)
}

// ---------------------------------------------------------------------------
// Pack index (v2) parsing.
// ---------------------------------------------------------------------------

/// Parse a pack index v2 (`*.idx`) into `(oid, pack offset)` pairs, in the
/// index's sorted-oid order.
///
/// # Errors
/// Returns [`GitError::Pack`] if the magic or version is wrong, if the file is
/// truncated relative to the object count the fanout table declares, or if a
/// large-offset reference points outside the large-offset table.
pub fn parse_idx(bytes: &[u8]) -> Result<Vec<(GitOid, u64)>, GitError> {
    if bytes.len() < 8 || bytes[..4] != IDX_MAGIC {
        return Err(GitError::Pack("pack index: bad magic (not v2)"));
    }
    if read_be_u32(bytes, 4)? != 2 {
        return Err(GitError::Pack(
            "pack index: unsupported version (expected 2)",
        ));
    }
    // Fanout: 256 big-endian u32; the last entry is the total object count.
    let fanout_start = 8;
    let count = read_be_u32(bytes, fanout_start + 255 * 4)? as usize;

    let names_start = fanout_start + 256 * 4;
    let crcs_start = names_start
        .checked_add(count.checked_mul(20).ok_or(OVERFLOW)?)
        .ok_or(OVERFLOW)?;
    let offsets_start = crcs_start
        .checked_add(count.checked_mul(4).ok_or(OVERFLOW)?)
        .ok_or(OVERFLOW)?;
    let large_start = offsets_start
        .checked_add(count.checked_mul(4).ok_or(OVERFLOW)?)
        .ok_or(OVERFLOW)?;
    if large_start > bytes.len() {
        return Err(GitError::Pack("pack index: truncated (offset table)"));
    }

    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let name_at = names_start + i * 20;
        let mut oid = [0u8; 20];
        oid.copy_from_slice(
            bytes
                .get(name_at..name_at + 20)
                .ok_or(GitError::Pack("pack index: truncated (name table)"))?,
        );
        let raw_offset = read_be_u32(bytes, offsets_start + i * 4)?;
        let offset = if raw_offset & IDX_LARGE_OFFSET_FLAG != 0 {
            let large_idx = (raw_offset & !IDX_LARGE_OFFSET_FLAG) as usize;
            let at = large_start
                .checked_add(large_idx.checked_mul(8).ok_or(OVERFLOW)?)
                .ok_or(OVERFLOW)?;
            read_be_u64(bytes, at)?
        } else {
            u64::from(raw_offset)
        };
        entries.push((GitOid::from_bytes(oid), offset));
    }
    Ok(entries)
}

/// A shared "index arithmetic overflow" error for the checked-math ladder above.
const OVERFLOW: GitError = GitError::Pack("pack index: size arithmetic overflow");

// ---------------------------------------------------------------------------
// Packfile parsing.
// ---------------------------------------------------------------------------

/// Git object type ids as stored in a pack entry header.
const OBJ_COMMIT: u8 = 1;
const OBJ_TREE: u8 = 2;
const OBJ_BLOB: u8 = 3;
const OBJ_TAG: u8 = 4;
const OBJ_OFS_DELTA: u8 = 6;
const OBJ_REF_DELTA: u8 = 7;

/// Validate the `PACK` header and return the object count.
///
/// # Errors
/// Returns [`GitError::Pack`] if the magic is wrong or the version is not 2/3.
fn parse_pack_header(bytes: &[u8]) -> Result<u32, GitError> {
    if bytes.len() < 12 || bytes[..4] != PACK_MAGIC {
        return Err(GitError::Pack("packfile: bad magic"));
    }
    let version = read_be_u32(bytes, 4)?;
    if version != 2 && version != 3 {
        return Err(GitError::Pack(
            "packfile: unsupported version (expected 2 or 3)",
        ));
    }
    read_be_u32(bytes, 8)
}

/// A single pack entry before delta resolution.
enum RawEntry {
    /// A whole object: its git type id (1–4) and inflated body.
    Base { type_id: u8, data: Vec<u8> },
    /// An offset delta: the absolute pack offset of the base plus the inflated
    /// delta instruction stream.
    OfsDelta { base_offset: u64, delta: Vec<u8> },
    /// A ref delta: the base object's oid plus the inflated delta stream.
    RefDelta { base_oid: GitOid, delta: Vec<u8> },
}

/// Read only the type id of the entry at `offset` (no inflate).
fn peek_type(pack: &[u8], offset: u64) -> Result<u8, GitError> {
    let mut pos =
        usize::try_from(offset).map_err(|_| GitError::Pack("packfile: offset overflow"))?;
    let (type_id, _) = read_entry_header(pack, &mut pos)?;
    Ok(type_id)
}

/// Parse the entry at `offset` into a [`RawEntry`] (inflating its body/delta).
fn parse_entry_at(pack: &[u8], offset: u64) -> Result<RawEntry, GitError> {
    let start = usize::try_from(offset).map_err(|_| GitError::Pack("packfile: offset overflow"))?;
    let mut pos = start;
    let (type_id, size) = read_entry_header(pack, &mut pos)?;
    match type_id {
        OBJ_COMMIT | OBJ_TREE | OBJ_BLOB | OBJ_TAG => {
            let data = inflate_at(pack, pos)?;
            if data.len() as u64 != size {
                return Err(GitError::Pack("packfile: object size mismatch"));
            }
            Ok(RawEntry::Base { type_id, data })
        }
        OBJ_OFS_DELTA => {
            let neg = read_ofs_delta_offset(pack, &mut pos)?;
            let base_offset = offset.checked_sub(neg).ok_or(GitError::Pack(
                "packfile: ofs-delta base before start of pack",
            ))?;
            let delta = inflate_at(pack, pos)?;
            if delta.len() as u64 != size {
                return Err(GitError::Pack("packfile: delta size mismatch"));
            }
            Ok(RawEntry::OfsDelta { base_offset, delta })
        }
        OBJ_REF_DELTA => {
            let end = pos
                .checked_add(20)
                .ok_or(GitError::Pack("packfile: offset overflow"))?;
            let oid_bytes = pack
                .get(pos..end)
                .ok_or(GitError::Pack("packfile: truncated ref-delta base oid"))?;
            let mut oid = [0u8; 20];
            oid.copy_from_slice(oid_bytes);
            pos = end;
            let delta = inflate_at(pack, pos)?;
            if delta.len() as u64 != size {
                return Err(GitError::Pack("packfile: delta size mismatch"));
            }
            Ok(RawEntry::RefDelta {
                base_oid: GitOid::from_bytes(oid),
                delta,
            })
        }
        _ => Err(GitError::Pack("packfile: unknown object type")),
    }
}

/// Resolve the entry at `offset` to `(git type id 1–4, object body bytes)`,
/// applying deltas recursively with memoization.
fn resolve(
    pack: &[u8],
    offset: u64,
    oid_to_offset: &HashMap<GitOid, u64>,
    memo: &mut HashMap<u64, (u8, Vec<u8>)>,
    depth: usize,
) -> Result<(u8, Vec<u8>), GitError> {
    if let Some(cached) = memo.get(&offset) {
        return Ok(cached.clone());
    }
    if depth > MAX_DELTA_DEPTH {
        return Err(GitError::Pack(
            "packfile: delta chain too deep (possible cycle)",
        ));
    }
    let resolved = match parse_entry_at(pack, offset)? {
        RawEntry::Base { type_id, data } => (type_id, data),
        RawEntry::OfsDelta { base_offset, delta } => {
            let (base_type, base_body) =
                resolve(pack, base_offset, oid_to_offset, memo, depth + 1)?;
            (base_type, apply_delta(&base_body, &delta)?)
        }
        RawEntry::RefDelta { base_oid, delta } => {
            let base_offset = *oid_to_offset
                .get(&base_oid)
                .ok_or(GitError::Pack("packfile: ref-delta base not in this pack"))?;
            let (base_type, base_body) =
                resolve(pack, base_offset, oid_to_offset, memo, depth + 1)?;
            (base_type, apply_delta(&base_body, &delta)?)
        }
    };
    memo.insert(offset, resolved.clone());
    Ok(resolved)
}

/// Wrap a resolved body as git's canonical `"{type} {len}\0{body}"` form so it
/// can be hashed and decoded by the shared object codec.
fn canonical_bytes(type_id: u8, body: &[u8]) -> Result<Vec<u8>, GitError> {
    let ty: &str = match type_id {
        OBJ_COMMIT => "commit",
        OBJ_TREE => "tree",
        OBJ_BLOB => "blob",
        OBJ_TAG => "tag",
        _ => return Err(GitError::Pack("packfile: delta base has non-object type")),
    };
    let len = body.len().to_string();
    let mut out = Vec::with_capacity(ty.len() + 1 + len.len() + 1 + body.len());
    out.extend_from_slice(ty.as_bytes());
    out.push(b' ');
    out.extend_from_slice(len.as_bytes());
    out.push(0);
    out.extend_from_slice(body);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Varint / offset / delta decoders.
// ---------------------------------------------------------------------------

/// Read a pack entry's `(type, size)` header: the first byte's bits `0x70` are
/// the type and its low nibble plus continuation bytes form the size.
fn read_entry_header(data: &[u8], pos: &mut usize) -> Result<(u8, u64), GitError> {
    let mut c = take_byte(data, pos)?;
    let type_id = (c >> 4) & 0x07;
    let mut size = u64::from(c & 0x0f);
    let mut shift = 4u32;
    while c & 0x80 != 0 {
        c = take_byte(data, pos)?;
        size |= u64::from(c & 0x7f)
            .checked_shl(shift)
            .ok_or(GitError::Pack("packfile: size varint overflow"))?;
        shift += 7;
        if size > MAX_OBJECT_SIZE {
            return Err(GitError::Pack("packfile: object size exceeds limit"));
        }
    }
    Ok((type_id, size))
}

/// Read git's base-128 negative offset (the `OFS_DELTA` base back-reference):
/// `off = c & 0x7f; while c & 0x80 { c = next; off = ((off+1)<<7) | (c & 0x7f) }`.
fn read_ofs_delta_offset(data: &[u8], pos: &mut usize) -> Result<u64, GitError> {
    let mut c = take_byte(data, pos)?;
    let mut off = u64::from(c & 0x7f);
    while c & 0x80 != 0 {
        c = take_byte(data, pos)?;
        off = off
            .checked_add(1)
            .and_then(|v| v.checked_shl(7))
            .map(|v| v | u64::from(c & 0x7f))
            .ok_or(GitError::Pack("packfile: ofs-delta offset overflow"))?;
    }
    Ok(off)
}

/// Read a little-endian base-128 size (the two sizes that open a delta stream).
fn read_delta_size(data: &[u8], pos: &mut usize) -> Result<u64, GitError> {
    let mut size = 0u64;
    let mut shift = 0u32;
    loop {
        let c = take_byte(data, pos)?;
        size |= u64::from(c & 0x7f)
            .checked_shl(shift)
            .ok_or(GitError::Pack("packfile: delta size varint overflow"))?;
        shift += 7;
        if c & 0x80 == 0 {
            break;
        }
        if size > MAX_OBJECT_SIZE {
            return Err(GitError::Pack("packfile: delta size exceeds limit"));
        }
    }
    Ok(size)
}

/// Apply a git delta stream to `base`, reconstructing the target bytes.
///
/// The stream opens with the source and target sizes (base-128 varints), then a
/// sequence of instructions: a *copy* (high bit set) names an offset/size range
/// of `base`; an *insert* (high bit clear, low 7 bits = length) supplies that
/// many literal bytes inline.
///
/// # Errors
/// Returns [`GitError::Pack`] if the declared source size does not match
/// `base`, if a copy range falls outside `base`, if an insert runs off the end
/// of the stream, on the reserved opcode `0`, or if the reconstructed length
/// does not match the declared target size.
pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, GitError> {
    let mut pos = 0usize;
    let src_size = read_delta_size(delta, &mut pos)?;
    if src_size != base.len() as u64 {
        return Err(GitError::Pack("delta: source size does not match base"));
    }
    let tgt_size = read_delta_size(delta, &mut pos)?;
    if tgt_size > MAX_OBJECT_SIZE {
        return Err(GitError::Pack("delta: target size exceeds limit"));
    }
    let mut out = Vec::with_capacity(tgt_size as usize);
    while pos < delta.len() {
        let opcode = take_byte(delta, &mut pos)?;
        if opcode & 0x80 != 0 {
            // Copy from base: assemble a 4-byte offset and 3-byte size from the
            // bytes selected by the low/high opcode bits.
            let mut cp_off = 0u64;
            for i in 0..4u32 {
                if opcode & (1 << i) != 0 {
                    cp_off |= u64::from(take_byte(delta, &mut pos)?) << (8 * i);
                }
            }
            let mut cp_size = 0u64;
            for i in 0..3u32 {
                if opcode & (1 << (4 + i)) != 0 {
                    cp_size |= u64::from(take_byte(delta, &mut pos)?) << (8 * i);
                }
            }
            if cp_size == 0 {
                cp_size = 0x1_0000;
            }
            let start = usize::try_from(cp_off)
                .map_err(|_| GitError::Pack("delta: copy offset overflow"))?;
            let end = start
                .checked_add(
                    usize::try_from(cp_size)
                        .map_err(|_| GitError::Pack("delta: copy size overflow"))?,
                )
                .ok_or(GitError::Pack("delta: copy range overflow"))?;
            let chunk = base
                .get(start..end)
                .ok_or(GitError::Pack("delta: copy range out of base bounds"))?;
            out.extend_from_slice(chunk);
        } else if opcode != 0 {
            // Insert: the opcode itself is the literal length (1..=127).
            let len = opcode as usize;
            let end = pos
                .checked_add(len)
                .ok_or(GitError::Pack("delta: insert length overflow"))?;
            let chunk = delta
                .get(pos..end)
                .ok_or(GitError::Pack("delta: insert runs past end of stream"))?;
            out.extend_from_slice(chunk);
            pos = end;
        } else {
            return Err(GitError::Pack("delta: reserved opcode 0"));
        }
    }
    if out.len() as u64 != tgt_size {
        return Err(GitError::Pack(
            "delta: reconstructed size does not match target",
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Byte-level helpers.
// ---------------------------------------------------------------------------

/// Take one byte at `*pos`, advancing it; error if out of range.
fn take_byte(data: &[u8], pos: &mut usize) -> Result<u8, GitError> {
    let b = *data
        .get(*pos)
        .ok_or(GitError::Pack("packfile: unexpected end of data"))?;
    *pos += 1;
    Ok(b)
}

/// Inflate the zlib stream at `start` and report how many **compressed** bytes
/// it consumed — the amount to advance to reach the next pack entry when there
/// is no index to supply offsets (the wire-pack sequential walk).
fn inflate_consumed(data: &[u8], start: usize) -> Result<usize, GitError> {
    let slice = data
        .get(start..)
        .ok_or(GitError::Pack("packfile: object starts past end of file"))?;
    let mut decoder = ZlibDecoder::new(slice);
    let mut sink = std::io::sink();
    std::io::copy(&mut decoder, &mut sink).map_err(|e| GitError::Zlib(e.to_string()))?;
    usize::try_from(decoder.total_in())
        .map_err(|_| GitError::Pack("packfile: compressed size overflow"))
}

/// Inflate the zlib stream that begins at `start` in `data`.
fn inflate_at(data: &[u8], start: usize) -> Result<Vec<u8>, GitError> {
    let slice = data
        .get(start..)
        .ok_or(GitError::Pack("packfile: object starts past end of file"))?;
    let mut decoder = ZlibDecoder::new(slice);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| GitError::Zlib(e.to_string()))?;
    Ok(out)
}

/// Read a big-endian `u32` at `at`.
fn read_be_u32(data: &[u8], at: usize) -> Result<u32, GitError> {
    let end = at
        .checked_add(4)
        .ok_or(GitError::Pack("read past end (u32)"))?;
    let b = data
        .get(at..end)
        .ok_or(GitError::Pack("read past end (u32)"))?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Read a big-endian `u64` at `at`.
fn read_be_u64(data: &[u8], at: usize) -> Result<u64, GitError> {
    let end = at
        .checked_add(8)
        .ok_or(GitError::Pack("read past end (u64)"))?;
    let b = data
        .get(at..end)
        .ok_or(GitError::Pack("read past end (u64)"))?;
    Ok(u64::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_header_single_byte() {
        // type=3 (blob), size=5, no continuation: (3<<4)|5 = 0x35.
        let mut pos = 0;
        let (ty, size) = read_entry_header(&[0x35], &mut pos).unwrap();
        assert_eq!((ty, size), (OBJ_BLOB, 5));
        assert_eq!(pos, 1);
    }

    #[test]
    fn entry_header_multi_byte_size() {
        // type=2 (tree), size=400. low nibble 0, continuation byte carries 25:
        // 0x80|(2<<4)|0 = 0xA0, then 0x19 (=25); size = 25<<4 = 400.
        let mut pos = 0;
        let (ty, size) = read_entry_header(&[0xA0, 0x19], &mut pos).unwrap();
        assert_eq!((ty, size), (OBJ_TREE, 400));
        assert_eq!(pos, 2);
    }

    #[test]
    fn ofs_delta_offset_vectors() {
        // Single byte: 0x0f -> 15.
        let mut pos = 0;
        assert_eq!(read_ofs_delta_offset(&[0x0f], &mut pos).unwrap(), 15);
        // Two bytes: [0x81, 0x00] -> ((1+1)<<7)|0 = 256.
        let mut pos = 0;
        assert_eq!(read_ofs_delta_offset(&[0x81, 0x00], &mut pos).unwrap(), 256);
    }

    #[test]
    fn delta_size_vectors() {
        // Single byte 0x05 -> 5.
        let mut pos = 0;
        assert_eq!(read_delta_size(&[0x05], &mut pos).unwrap(), 5);
        // [0x81, 0x01] -> 1 | (1<<7) = 129.
        let mut pos = 0;
        assert_eq!(read_delta_size(&[0x81, 0x01], &mut pos).unwrap(), 129);
    }

    #[test]
    fn delta_applies_copy_and_insert() {
        // base = "hello world"; rebuild "hello there world" with:
        //   copy base[0..6] ("hello "), insert "there ", copy base[6..11] ("world").
        let base = b"hello world";
        let delta: &[u8] = &[
            0x0b, // source size = 11
            0x11, // target size = 17
            0x90, 0x06, // copy: offset 0 (no offset bytes), size byte 0x06
            0x06, b't', b'h', b'e', b'r', b'e', b' ', // insert 6 literal bytes
            0x91, 0x06, 0x05, // copy: offset byte 0x06, size byte 0x05
        ];
        let out = apply_delta(base, delta).unwrap();
        assert_eq!(out, b"hello there world");
    }

    #[test]
    fn delta_copy_zero_size_means_0x10000() {
        // A copy with size == 0 means 0x10000 bytes. Build a base that large.
        let base = vec![7u8; 0x1_0000];
        // source size 0x10000 varint: 0x80,0x80,0x04. target size same.
        let delta: &[u8] = &[
            0x80, 0x80, 0x04, // source size = 65536
            0x80, 0x80, 0x04, // target size = 65536
            0x80, // copy: no offset, no size bytes -> size defaults to 0x10000
        ];
        let out = apply_delta(&base, delta).unwrap();
        assert_eq!(out.len(), 0x1_0000);
        assert!(out.iter().all(|&b| b == 7));
    }

    #[test]
    fn delta_rejects_source_size_mismatch() {
        // Declares source size 99 but base is 2 bytes.
        assert!(apply_delta(b"hi", &[99, 1, 0x01, b'x']).is_err());
    }

    #[test]
    fn delta_rejects_copy_out_of_range() {
        // base 2 bytes; copy offset 0 size 5 is out of range.
        let delta: &[u8] = &[0x02, 0x05, 0x90, 0x05];
        assert!(apply_delta(b"hi", delta).is_err());
    }

    #[test]
    fn delta_rejects_reserved_opcode() {
        let delta: &[u8] = &[0x00, 0x00, 0x00];
        assert!(apply_delta(b"", delta).is_err());
    }

    #[test]
    fn idx_rejects_bad_magic() {
        assert!(parse_idx(b"not an index at all!!").is_err());
    }

    #[test]
    fn pack_header_rejects_bad_magic() {
        assert!(parse_pack_header(b"NOPExxxxxxxx").is_err());
    }

    /// zlib-deflate a byte slice, for building an in-memory pack in a test.
    fn deflate(data: &[u8]) -> Vec<u8> {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write as _;
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Frame one whole (non-delta) object into a pack entry: the varint
    /// `(type, size)` header followed by the deflated body.
    fn whole_entry(type_id: u8, body: &[u8]) -> Vec<u8> {
        let size = body.len();
        // Single-byte header path (bodies here are < 16 bytes): (type<<4)|size.
        assert!(
            size < 16,
            "test bodies stay in the single-header-byte range"
        );
        let mut out = vec![((type_id & 0x07) << 4) | (size as u8 & 0x0f)];
        out.extend_from_slice(&deflate(body));
        out
    }

    #[test]
    fn parse_pack_bytes_walks_a_self_contained_pack() {
        // Build a two-object pack (a blob and a blob) with no index and a trailing
        // SHA-1 — the exact shape upload-pack streams over the wire.
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        pack.extend_from_slice(&2u32.to_be_bytes());
        pack.extend_from_slice(&2u32.to_be_bytes()); // count = 2
        pack.extend_from_slice(&whole_entry(OBJ_BLOB, b"hi"));
        pack.extend_from_slice(&whole_entry(OBJ_BLOB, b"bye"));
        // Trailing pack checksum (ignored by the sequential walk).
        pack.extend_from_slice(&sha1_bytes(&pack));

        let objects = parse_pack_bytes(&pack).unwrap();
        assert_eq!(objects.len(), 2);
        // Objects come back in pack order, oids recomputed from content.
        assert_eq!(objects[0].1, GitObject::Blob(b"hi".to_vec()));
        assert_eq!(objects[1].1, GitObject::Blob(b"bye".to_vec()));
        assert_eq!(objects[0].0, crate::object::oid(&objects[0].1));
        assert_eq!(objects[1].0, crate::object::oid(&objects[1].1));
    }

    #[test]
    fn parse_pack_bytes_rejects_bad_magic() {
        assert!(parse_pack_bytes(b"NOPE\0\0\0\x02\0\0\0\0").is_err());
    }

    #[test]
    fn pack_header_reads_count() {
        // "PACK", version 2, count 3.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PACK");
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&3u32.to_be_bytes());
        assert_eq!(parse_pack_header(&bytes).unwrap(), 3);
    }
}
