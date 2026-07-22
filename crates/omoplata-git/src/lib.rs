//! Git interoperability for omoplata — the I9 round-trip gate.
//!
//! Per the design doc's §7 crate table this crate (`omoplata-git`, crate #6)
//! owns the *"Git object format + wire protocol; round-trip fuzz gate — I9"*.
//! It is marked **"Unverified, mandatory"**: the round-trip guarantee is
//! discharged empirically, not with a Verus proof.
//!
//! ## The principle (§3 P8)
//! *"Git interoperability is non-negotiable. omoplata reads and writes the git
//! object format and wire protocol. Round-trip fidelity (`git repo → import →
//! export → bit-identical`) is a release gate. … omoplata smuggles the
//! revolution in as a backend."*
//!
//! ## The invariant (§6 I9)
//! *"Round-trip fidelity (tested, not proven): `export(import(git_repo)) ≡
//! git_repo` bit-identically, held as a fuzz-tested release gate rather than a
//! Verus theorem (the git format's warts resist clean modeling)."*
//!
//! ## Scope (§8)
//! In v1: *"git interop with round-trip gate."* Explicitly out: *"SHA-1 interop
//! beyond what git import requires"* — this crate implements exactly the SHA-1
//! object addressing git import needs, no more.
//!
//! ## What this crate provides
//! - A faithful git object codec ([`encode`], [`decode`], [`oid`]) over
//!   [`GitObject`] — trees, commits, and tags all re-encode byte-identically.
//!   Commits and tags are parsed into typed graph fields ([`GitCommit`],
//!   [`GitTag`]) while retaining their raw body for exact re-encoding.
//! - Loose-object I/O ([`read_loose`], [`write_loose`], [`walk_loose`]).
//! - Packfile + delta decoding ([`read_pack`], [`read_all_packs`]): pack index
//!   v2 parsing, `OFS_DELTA`/`REF_DELTA` resolution, and delta application, so a
//!   `git gc`'d repository imports and verifies through the same I9 gate as
//!   loose objects.
//! - Ref reading ([`read_refs`]): `HEAD`, loose refs, and `packed-refs`.
//! - Git-directory resolution ([`resolve_git_dir`]): maps a worktree root or a
//!   git directory to the directory that actually holds `objects/`+`refs/`, so
//!   `omo git verify`/`import` auto-descend into `.git` instead of silently
//!   walking a non-existent `objects/` and reporting a vacuous PASS.
//! - The round-trip gate: [`roundtrip_ok`] for one object and [`verify_repo`]
//!   for a whole repository — the executable form of I9. `verify_repo` reports
//!   PASS only when ≥1 object was actually checked; an empty or non-repository
//!   path is refused, never PASSed.
//! - Commit-graph import ([`import_repo`]): walks the commit DAG from refs,
//!   importing every reachable object through the I9 gate and recording the DAG.
//! - Exact-mode export ([`export_repo`]) and the repo-level round-trip gate
//!   ([`export_matches_source`]) — the outbound half of I9.
//! - The **git wire protocol** over the local transport ([`fetch_local`]): a real
//!   pkt-line codec ([`write_pkt_line`], [`read_pkt_line`]) and an `upload-pack`
//!   fetch client that clones a `file://`/local repo over `git upload-pack`,
//!   decodes the received packfile in memory ([`parse_pack_bytes`]), and imports
//!   it through the I9 gate ([`import_objects`]). This is the design doc's §3 P8
//!   *"reads and writes the git object format **and wire protocol**"* — see the
//!   crate ADR for the local-vs-networked-transport scope.
//!
//! No `unwrap`/`expect`/`panic` appears outside tests.

mod error;
mod export;
mod gate;
mod gitdir;
mod import;
mod loose;
mod object;
mod pack;
mod refs;
mod wire;

pub use error::GitError;
pub use export::{export_matches_source, export_repo, GitExport};
pub use gate::{roundtrip_ok, verify_repo, GitReport};
pub use gitdir::resolve_git_dir;
pub use import::{import_objects, import_repo, mode_to_kind, GitImport};
pub use loose::{
    loose_path, oid_from_loose_path, pack_file_count, read_loose, walk_loose, write_loose,
};
pub use object::{decode, encode, oid, GitCommit, GitObject, GitOid, GitTag, GitTreeEntry};
pub use pack::{
    apply_delta, pack_paths, parse_idx, parse_pack_bytes, read_all_packs, read_pack,
    read_pack_detailed, PackDecode,
};
pub use refs::read_refs;
pub use wire::{
    decode_wire_pack, fetch_local, read_pkt, read_pkt_line, write_flush, write_pkt_line, PktLine,
    WireFetch, FLUSH_PKT, MAX_PAYLOAD,
};

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// I9 property: for arbitrary blob bytes, the gate accepts the encoded
        /// object, returns its oid, and the encoding is byte-identical.
        #[test]
        fn blob_roundtrip_gate(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let object = GitObject::Blob(bytes.clone());
            let encoded = encode(&object);
            let gate_oid = roundtrip_ok(&encoded)?;
            prop_assert_eq!(gate_oid, oid(&object));
            // Byte-identical: encode(decode(encoded)) == encoded.
            prop_assert_eq!(encode(&decode(&encoded)?), encoded);
        }

        /// I9 property over trees: arbitrary entries re-encode byte-identically
        /// and preserve order.
        #[test]
        fn tree_roundtrip_gate(
            entries in proptest::collection::vec(
                (
                    prop_oneof![
                        Just("100644".to_owned()),
                        Just("100755".to_owned()),
                        Just("120000".to_owned()),
                        Just("40000".to_owned()),
                    ],
                    "[a-zA-Z0-9_.-]{1,16}",
                    any::<[u8; 20]>(),
                ),
                0..12,
            )
        ) {
            let entries: Vec<GitTreeEntry> = entries
                .into_iter()
                .map(|(mode, name, oid)| GitTreeEntry { mode, name, oid })
                .collect();
            let tree = GitObject::Tree(entries.clone());
            let encoded = encode(&tree);
            roundtrip_ok(&encoded)?;
            prop_assert_eq!(decode(&encoded)?, GitObject::Tree(entries));
        }
    }
}
