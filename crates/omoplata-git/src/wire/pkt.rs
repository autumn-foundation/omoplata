//! The git **pkt-line** framing codec.
//!
//! Every message in the git wire protocol (`upload-pack`/`receive-pack`, over
//! any transport) is framed as a *pkt-line*: a 4-character hexadecimal length
//! prefix followed by that many bytes of payload — where the length **includes**
//! the 4-byte prefix itself. Three lengths are special "magic" packets that
//! carry no payload:
//!
//! | bytes  | meaning                                    |
//! |--------|--------------------------------------------|
//! | `0000` | **flush-pkt** — end of a message section   |
//! | `0001` | **delim-pkt** — section delimiter (proto v2) |
//! | `0002` | **response-end-pkt** — end of response (v2) |
//!
//! So a data line carrying `"hello\n"` (6 bytes) is framed as
//! `"000a" + "hello\n"` — length `0x000a == 10 == 6 + 4`. This module is the
//! transport-agnostic reader/writer for that framing; the `upload-pack` client
//! ([`crate::fetch_local`]) is built on top of it.
//!
//! No `unwrap`/`expect`/`panic` appears outside tests.

use std::io::{Read, Write};

use crate::error::GitError;

/// The flush-pkt (`0000`): the end-of-section marker.
pub const FLUSH_PKT: &[u8; 4] = b"0000";

/// The largest payload a single pkt-line can carry: the 16-bit length field caps
/// the whole frame at `0xffff`, leaving `0xffff - 4` bytes of payload.
pub const MAX_PAYLOAD: usize = 0xffff - 4;

/// A decoded pkt-line: either a data line or one of the three magic markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PktLine {
    /// A data line and its payload bytes (the length prefix stripped).
    Data(Vec<u8>),
    /// The flush-pkt `0000` — end of a message section.
    Flush,
    /// The delim-pkt `0001` — a section delimiter (protocol v2).
    Delim,
    /// The response-end-pkt `0002` — end of a response (protocol v2).
    ResponseEnd,
}

/// Write a single **data** pkt-line: the 4-hex length prefix (payload length + 4)
/// followed by `payload`.
///
/// For example `write_pkt_line(w, b"hello\n")` emits `b"000ahello\n"`.
///
/// # Errors
/// Returns [`GitError::WireProto`] if `payload` exceeds [`MAX_PAYLOAD`], or
/// [`GitError::Wire`] if the underlying writer fails.
pub fn write_pkt_line<W: Write>(w: &mut W, payload: &[u8]) -> Result<(), GitError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(GitError::WireProto("pkt-line payload exceeds 65531 bytes"));
    }
    let len = payload.len() + 4;
    // `len <= 0xffff` holds because `payload.len() <= MAX_PAYLOAD`.
    let header = format!("{len:04x}");
    w.write_all(header.as_bytes()).map_err(wire_io)?;
    w.write_all(payload).map_err(wire_io)?;
    Ok(())
}

/// Write a flush-pkt (`0000`).
///
/// # Errors
/// Returns [`GitError::Wire`] if the underlying writer fails.
pub fn write_flush<W: Write>(w: &mut W) -> Result<(), GitError> {
    w.write_all(FLUSH_PKT).map_err(wire_io)
}

/// Read one pkt-line, classifying it as data or a magic marker.
///
/// # Errors
/// Returns [`GitError::WireProto`] on a malformed length prefix (non-hex, or the
/// reserved length `3`) or a truncated payload, or [`GitError::Wire`] if the
/// underlying reader fails before a complete frame is read.
pub fn read_pkt<R: Read>(r: &mut R) -> Result<PktLine, GitError> {
    let mut header = [0u8; 4];
    read_exact(r, &mut header)?;
    let len = parse_hex4(&header)?;
    match len {
        0 => Ok(PktLine::Flush),
        1 => Ok(PktLine::Delim),
        2 => Ok(PktLine::ResponseEnd),
        3 => Err(GitError::WireProto("pkt-line: reserved length 3")),
        _ => {
            let mut payload = vec![0u8; len - 4];
            read_exact(r, &mut payload)?;
            Ok(PktLine::Data(payload))
        }
    }
}

/// Read one pkt-line, returning `Some(payload)` for a data line and `None` for
/// any section terminator (flush / delim / response-end).
///
/// This is the convenience shape the ref-advertisement and negotiation loops use
/// (`None` ends the section).
///
/// # Errors
/// See [`read_pkt`].
pub fn read_pkt_line<R: Read>(r: &mut R) -> Result<Option<Vec<u8>>, GitError> {
    match read_pkt(r)? {
        PktLine::Data(payload) => Ok(Some(payload)),
        PktLine::Flush | PktLine::Delim | PktLine::ResponseEnd => Ok(None),
    }
}

/// Parse a git ref-advertisement line `"<40-hex-oid> <refname>"` into its oid and
/// ref name, tolerating a trailing `\n`. The capabilities segment (everything
/// after a `\0` on the first advertised line) must be stripped by the caller
/// before calling this.
///
/// # Errors
/// Returns [`GitError::WireProto`] if the line has no space separator or the oid
/// is not 40 lowercase hex characters.
pub fn parse_ref_line(line: &[u8]) -> Result<(crate::object::GitOid, String), GitError> {
    let line = trim_trailing_newline(line);
    let sp = line
        .iter()
        .position(|&b| b == b' ')
        .ok_or(GitError::WireProto("ref advertisement line has no space"))?;
    let oid_hex = std::str::from_utf8(&line[..sp])
        .map_err(|_| GitError::WireProto("ref advertisement oid is not ascii"))?;
    let oid = crate::object::GitOid::from_hex(oid_hex)
        .map_err(|_| GitError::WireProto("ref advertisement oid is not 40-hex"))?;
    let name = std::str::from_utf8(&line[sp + 1..])
        .map_err(|_| GitError::WireProto("ref name is not utf-8"))?
        .to_owned();
    Ok((oid, name))
}

/// Strip a single trailing `\n` (if present) from a byte slice.
pub fn trim_trailing_newline(line: &[u8]) -> &[u8] {
    match line.last() {
        Some(b'\n') => &line[..line.len() - 1],
        _ => line,
    }
}

/// Parse a 4-byte ASCII lowercase-hex length prefix into a `usize`.
fn parse_hex4(bytes: &[u8; 4]) -> Result<usize, GitError> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| GitError::WireProto("pkt-line length prefix is not ascii"))?;
    usize::from_str_radix(s, 16)
        .map_err(|_| GitError::WireProto("pkt-line length prefix is not hex"))
}

/// Read exactly `buf.len()` bytes, mapping EOF and I/O errors into wire errors.
fn read_exact<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<(), GitError> {
    r.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            GitError::Wire("unexpected end of stream reading a pkt-line".to_owned())
        } else {
            wire_io(e)
        }
    })
}

/// Map an I/O error from the pkt-line transport into [`GitError::Wire`].
fn wire_io(e: std::io::Error) -> GitError {
    GitError::Wire(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_data_line_has_length_prefix() {
        // The canonical vector: "hello\n" (6 bytes) frames as "000ahello\n".
        let mut buf = Vec::new();
        write_pkt_line(&mut buf, b"hello\n").unwrap();
        assert_eq!(buf, b"000ahello\n");
    }

    #[test]
    fn write_flush_is_0000() {
        let mut buf = Vec::new();
        write_flush(&mut buf).unwrap();
        assert_eq!(buf, b"0000");
    }

    #[test]
    fn empty_payload_frames_as_0004() {
        let mut buf = Vec::new();
        write_pkt_line(&mut buf, b"").unwrap();
        assert_eq!(buf, b"0004");
    }

    #[test]
    fn read_data_line_round_trips() {
        let mut buf = Vec::new();
        write_pkt_line(&mut buf, b"want abc\n").unwrap();
        let mut cur = std::io::Cursor::new(buf);
        assert_eq!(
            read_pkt_line(&mut cur).unwrap(),
            Some(b"want abc\n".to_vec())
        );
    }

    #[test]
    fn read_flush_is_none() {
        let mut cur = std::io::Cursor::new(b"0000".to_vec());
        assert_eq!(read_pkt_line(&mut cur).unwrap(), None);
        // And the typed reader classifies it precisely.
        let mut cur = std::io::Cursor::new(b"0000".to_vec());
        assert_eq!(read_pkt(&mut cur).unwrap(), PktLine::Flush);
    }

    #[test]
    fn read_classifies_delim_and_response_end() {
        let mut cur = std::io::Cursor::new(b"0001".to_vec());
        assert_eq!(read_pkt(&mut cur).unwrap(), PktLine::Delim);
        let mut cur = std::io::Cursor::new(b"0002".to_vec());
        assert_eq!(read_pkt(&mut cur).unwrap(), PktLine::ResponseEnd);
    }

    #[test]
    fn read_sequence_of_lines_then_flush() {
        let mut buf = Vec::new();
        write_pkt_line(&mut buf, b"line one\n").unwrap();
        write_pkt_line(&mut buf, b"line two\n").unwrap();
        write_flush(&mut buf).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        let mut lines = Vec::new();
        while let Some(l) = read_pkt_line(&mut cur).unwrap() {
            lines.push(l);
        }
        assert_eq!(lines, vec![b"line one\n".to_vec(), b"line two\n".to_vec()]);
    }

    #[test]
    fn ref_advertisement_line_round_trips() {
        // A real-shaped advertisement data line for a branch ref.
        let oid_hex = "df8fd4186be619a736a6bbe1f3ce3894cc149483";
        let framed = format!("{oid_hex} refs/heads/main\n");
        let mut buf = Vec::new();
        write_pkt_line(&mut buf, framed.as_bytes()).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        let payload = read_pkt_line(&mut cur).unwrap().unwrap();
        let (oid, name) = parse_ref_line(&payload).unwrap();
        assert_eq!(oid.hex(), oid_hex);
        assert_eq!(name, "refs/heads/main");
    }

    #[test]
    fn parse_ref_line_strips_capabilities_caller_side() {
        // The caller strips the "\0<caps>" tail before parse_ref_line; here we
        // confirm parse_ref_line rejects a raw first line that still has the NUL.
        let line = b"df8fd4186be619a736a6bbe1f3ce3894cc149483 HEAD\0multi_ack ofs-delta";
        // The name would include the NUL+caps, so the oid still parses but the
        // name is not a clean ref — callers must split on NUL first.
        let (_oid, name) = parse_ref_line(line).unwrap();
        assert!(name.contains('\u{0}'));
    }

    #[test]
    fn reject_bad_length_prefix() {
        let mut cur = std::io::Cursor::new(b"zzzz".to_vec());
        assert!(read_pkt(&mut cur).is_err());
    }

    #[test]
    fn reject_reserved_length_three() {
        let mut cur = std::io::Cursor::new(b"0003".to_vec());
        assert!(read_pkt(&mut cur).is_err());
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let mut buf = Vec::new();
        let big = vec![b'x'; MAX_PAYLOAD + 1];
        assert!(write_pkt_line(&mut buf, &big).is_err());
    }
}
