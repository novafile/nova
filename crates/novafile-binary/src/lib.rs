//! NOVA binary frames and streams (spec §5.4–5.5).
//!
//! ```text
//! frame   = magic "NOVA" | version 0x01 | flags u8
//!           | [schema: varint len, UTF-8]      (flags & SCHEMA)
//!           | payload-length varint | payload  (a §5.1 value encoding)
//!           | [crc32: u32 big-endian of payload] (flags & CRC32)
//! stream  = frame*
//! ```

use novafile_value::{DecodeError, EncodeOptions, Limits, Value, decode_prefix, encode};
use std::fmt;

pub const MAGIC: &[u8; 4] = b"NOVA";
pub const VERSION: u8 = 0x01;

pub mod flags {
    /// A schema reference follows the flags byte.
    pub const SCHEMA: u8 = 0b0000_0001;
    /// The payload is in canonical form (sorted, no back-references).
    pub const CANONICAL: u8 = 0b0000_0010;
    /// A CRC-32 of the payload follows it.
    pub const CRC32: u8 = 0b0000_0100;
    /// The payload may contain string back-references (informational).
    pub const DEDUPE: u8 = 0b0000_1000;
    /// The payload may contain record lists (informational).
    pub const TABLES: u8 = 0b0001_0000;
    pub const KNOWN: u8 = 0b0001_1111;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Schema reference such as `infrastructure@1.0.0#8c21a0f3d5e6b7c8`.
    pub schema: Option<String>,
    pub value: Value,
}

#[derive(Clone, Copy, Debug)]
#[expect(clippy::struct_excessive_bools, reason = "independent encoder switches; public API")]
pub struct FrameOptions {
    pub canonical: bool,
    pub dedupe_strings: bool,
    /// Encode lists of same-shaped records as record lists (spec §5.3).
    pub tables: bool,
    pub checksum: bool,
}

impl Default for FrameOptions {
    fn default() -> Self {
        FrameOptions {
            canonical: false,
            dedupe_strings: true,
            tables: true,
            checksum: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Truncated,
    BadMagic,
    UnsupportedVersion(u8),
    UnknownFlags(u8),
    Checksum { expected: u32, actual: u32 },
    Payload(DecodeError),
    Malformed(String),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Truncated => write!(f, "truncated frame"),
            FrameError::BadMagic => write!(f, "missing NOVA magic"),
            FrameError::UnsupportedVersion(v) => write!(f, "unsupported frame version {v}"),
            FrameError::UnknownFlags(x) => write!(f, "unknown frame flags 0x{x:02X}"),
            FrameError::Checksum { expected, actual } => write!(f, "CRC mismatch: expected {expected:08x}, got {actual:08x}"),
            FrameError::Payload(e) => write!(f, "payload: {e}"),
            FrameError::Malformed(m) => write!(f, "malformed frame: {m}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[expect(clippy::cast_possible_truncation, reason = "LEB128: only the low 7 bits are kept per byte")]
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(buf: &[u8], pos: &mut usize) -> Result<u64, FrameError> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos).ok_or(FrameError::Truncated)?;
        *pos += 1;
        v |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(FrameError::Malformed("varint too long".into()))
}

#[must_use]
pub fn encode_frame(frame: &Frame, opts: FrameOptions) -> Vec<u8> {
    let payload = encode(
        &frame.value,
        EncodeOptions {
            canonical: opts.canonical,
            dedupe_strings: opts.dedupe_strings && !opts.canonical,
            tables: opts.tables && !opts.canonical,
        },
    );
    let mut f = 0u8;
    if frame.schema.is_some() {
        f |= flags::SCHEMA;
    }
    if opts.canonical {
        f |= flags::CANONICAL;
    } else {
        if opts.dedupe_strings {
            f |= flags::DEDUPE;
        }
        if opts.tables {
            f |= flags::TABLES;
        }
    }
    if opts.checksum {
        f |= flags::CRC32;
    }
    let mut out = Vec::with_capacity(payload.len() + 16);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(f);
    if let Some(s) = &frame.schema {
        put_varint(&mut out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }
    put_varint(&mut out, payload.len() as u64);
    out.extend_from_slice(&payload);
    if opts.checksum {
        out.extend_from_slice(&crc32(&payload).to_be_bytes());
    }
    out
}

/// Decode one frame from the start of `buf`; returns the frame and bytes consumed.
///
/// # Errors
///
/// Returns [`FrameError`] if the buffer is truncated, has the wrong magic or an
/// unsupported version, sets unknown flags, has a non-UTF-8 schema reference,
/// fails its CRC-32 check, or carries a payload that does not decode.
pub fn decode_frame(buf: &[u8]) -> Result<(Frame, usize), FrameError> {
    if buf.len() < 6 {
        return Err(if buf.len() >= 4 && &buf[..4] != MAGIC {
            FrameError::BadMagic
        } else {
            FrameError::Truncated
        });
    }
    if &buf[..4] != MAGIC {
        return Err(FrameError::BadMagic);
    }
    if buf[4] != VERSION {
        return Err(FrameError::UnsupportedVersion(buf[4]));
    }
    let f = buf[5];
    if f & !flags::KNOWN != 0 {
        return Err(FrameError::UnknownFlags(f));
    }
    let mut pos = 6;
    let schema = if f & flags::SCHEMA != 0 {
        let n = usize::try_from(get_varint(buf, &mut pos)?).map_err(|_| FrameError::Truncated)?;
        let s = buf.get(pos..pos.checked_add(n).ok_or(FrameError::Truncated)?).ok_or(FrameError::Truncated)?;
        pos += n;
        Some(String::from_utf8(s.to_vec()).map_err(|_| FrameError::Malformed("schema reference is not UTF-8".into()))?)
    } else {
        None
    };
    let len = usize::try_from(get_varint(buf, &mut pos)?).map_err(|_| FrameError::Truncated)?;
    let end = pos.checked_add(len).ok_or(FrameError::Truncated)?;
    let payload = buf.get(pos..end).ok_or(FrameError::Truncated)?;
    pos = end;
    if f & flags::CRC32 != 0 {
        let c: [u8; 4] = buf.get(pos..pos + 4).and_then(|c| c.try_into().ok()).ok_or(FrameError::Truncated)?;
        pos += 4;
        let expected = u32::from_be_bytes(c);
        let actual = crc32(payload);
        if expected != actual {
            return Err(FrameError::Checksum { expected, actual });
        }
    }
    let (value, used) = decode_prefix(payload, Limits::default()).map_err(FrameError::Payload)?;
    if used != payload.len() {
        return Err(FrameError::Malformed("payload length does not match value".into()));
    }
    Ok((Frame { schema, value }, pos))
}

/// Iterates frames in a byte stream. With `recover`, a corrupt frame is
/// reported once and the reader resynchronizes at the next `NOVA` magic
/// (spec §5.5); without it, iteration stops at the first error.
pub struct FrameReader<'a> {
    buf: &'a [u8],
    pos: usize,
    recover: bool,
    failed: bool,
}

impl<'a> FrameReader<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        FrameReader {
            buf,
            pos: 0,
            recover: false,
            failed: false,
        }
    }

    #[must_use]
    pub fn recovering(mut self) -> Self {
        self.recover = true;
        self
    }
}

impl Iterator for FrameReader<'_> {
    type Item = Result<Frame, (usize, FrameError)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.buf.len() {
            return None;
        }
        let at = self.pos;
        match decode_frame(&self.buf[at..]) {
            Ok((f, n)) => {
                self.pos += n;
                Some(Ok(f))
            }
            Err(e) => {
                if self.recover {
                    let rest = &self.buf[at + 1..];
                    self.pos = match rest.windows(4).position(|w| w == MAGIC) {
                        Some(i) => at + 1 + i,
                        None => self.buf.len(),
                    };
                } else {
                    self.failed = true;
                }
                Some(Err((at, e)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use novafile_value::Record;

    fn val(i: i64) -> Value {
        let mut r = Record::new();
        r.insert("id", Value::String(format!("svc-{i}"))).unwrap();
        r.insert("replicas", i.into()).unwrap();
        Value::Record(r)
    }

    #[test]
    fn crc_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn frame_round_trip() {
        let f = Frame {
            schema: Some("infrastructure@1.0.0".into()),
            value: val(3),
        };
        for opts in [
            FrameOptions::default(),
            FrameOptions {
                canonical: true,
                dedupe_strings: false,
                tables: false,
                checksum: false,
            },
        ] {
            let bytes = encode_frame(&f, opts);
            let (g, n) = decode_frame(&bytes).unwrap();
            assert_eq!(n, bytes.len());
            assert_eq!(g, f);
        }
    }

    #[test]
    fn stream_recovery() {
        let mut stream = Vec::new();
        for i in 0..3 {
            stream.extend(encode_frame(&Frame { schema: None, value: val(i) }, FrameOptions::default()));
        }
        let second = encode_frame(&Frame { schema: None, value: val(0) }, FrameOptions::default()).len();
        stream[second + 9] ^= 0xFF; // corrupt the second frame's payload
        let all: Vec<_> = FrameReader::new(&stream).recovering().collect();
        assert_eq!(all.len(), 3);
        assert!(all[0].is_ok() && all[1].is_err() && all[2].is_ok());
        assert_eq!(all[2].as_ref().unwrap().value, val(2));
        let strict: Vec<_> = FrameReader::new(&stream).collect();
        assert_eq!(strict.len(), 2);
    }
}
