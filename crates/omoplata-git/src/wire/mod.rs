//! The git **wire protocol** over the local (`file://`) transport (§3 P8).
//!
//! Design doc §3 P8: *"Git interoperability is non-negotiable. omoplata reads and
//! writes the git object format **and wire protocol**."* This module implements
//! the wire protocol's fetch half — a real pkt-line + `upload-pack` client — over
//! the **local transport** git uses for `file://` URLs and local paths: the
//! client spawns the server-side `git upload-pack` process and speaks pkt-line
//! over its stdio. It is the same protocol code the networked (`http`/`ssh`)
//! transports would drive; only the process/socket plumbing differs, and the
//! networked transports (not offline-testable here) are documented as future work
//! in the crate ADR.
//!
//! - [`pkt`] — the pkt-line framing codec (`write_pkt_line`/`read_pkt_line`,
//!   flush/delim/response-end handling, ref-line parsing).
//! - [`fetch`] — the `upload-pack` fetch client ([`fetch_local`]): ref
//!   advertisement, `want`/`done` negotiation, raw packfile receipt, and import
//!   through the I9 gate.
//!
//! Push (`receive-pack`) over the local transport is **not** implemented in this
//! reduction; see the crate ADR for its status and the wire-protocol future work.

pub mod fetch;
pub mod pkt;

pub use fetch::{decode_wire_pack, fetch_local, WireFetch};
pub use pkt::{
    read_pkt, read_pkt_line, write_flush, write_pkt_line, PktLine, FLUSH_PKT, MAX_PAYLOAD,
};
