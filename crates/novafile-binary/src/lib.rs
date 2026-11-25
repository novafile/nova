//! NOVA binary frames and streams (spec §5.4–5.5).
//!
//! ```text
//! frame   = magic "NOVA" | version 0x01 | flags u8
//!           | [schema: varint len, UTF-8]      (flags & SCHEMA)
//!           | payload-length varint | payload  (a §5.1 value encoding)
//!           | [crc32: u32 big-endian of payload] (flags & CRC32)
//! stream  = frame*
//! ```

pub mod stream;

use novafile_value::{DecodeError, EncodeOptions, Limits, Value, decode_prefix, encode};
use std::fmt;

/// The four magic bytes every frame starts with; also the resynchronization marker.
pub const MAGIC: &[u8; 4] = b"NOVA";
/// The frame format version this crate reads and writes.
pub const VERSION: u8 = 0x01;

/// Bits of the frame flags byte.
pub mod flags {
    /// A schema reference follows the flags byte.
    pub const SCHEMA: u8 = 0b0000_0001;
    /// The writer claims the payload is in canonical form (sorted, no back-references).
    ///
    /// Informational: the decoder does not verify it, and [`crate::Frame`]
    /// does not expose it. To rely on canonical bytes, re-encode the value with
    /// [`novafile_value::canonical_bytes`].
    pub const CANONICAL: u8 = 0b0000_0010;
    /// A CRC-32 of the payload follows it.
    pub const CRC32: u8 = 0b0000_0100;
    /// The payload may contain string back-references (informational).
    pub const DEDUPE: u8 = 0b0000_1000;
    /// The payload may contain record lists (informational).
    pub const TABLES: u8 = 0b0001_0000;
    /// The payload is a stream-control value (`!header{…}`, `!end{…}`, …), not data.
    pub const META: u8 = 0b0010_0000;
    /// All flags defined by this version; a frame setting any other bit is rejected.
    pub const KNOWN: u8 = 0b0011_1111;
}

/// One frame: an optional schema reference and a value.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Schema reference such as `infrastructure@1.0.0#8c21a0f3d5e6b7c8`.
    pub schema: Option<String>,
    /// The framed value.
    pub value: Value,
    /// The value is stream control (a META frame), not part of the data a stream denotes.
    pub meta: bool,
}

/// Encoder switches for [`encode_frame`]. The default dedupes strings, writes
/// record lists and appends a CRC-32.
#[derive(Clone, Copy, Debug)]
#[expect(clippy::struct_excessive_bools, reason = "independent encoder switches; public API")]
pub struct FrameOptions {
    /// Write the payload in canonical form (spec §5.2); disables `dedupe_strings` and `tables`.
    pub canonical: bool,
    /// Replace repeated strings with back-references (spec §5.3). Ignored when `canonical`.
    pub dedupe_strings: bool,
    /// Encode lists of same-shaped records as record lists (spec §5.3).
    pub tables: bool,
    /// Append a CRC-32 of the payload.
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

/// A failure to decode a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The input ends before the frame does.
    Truncated,
    /// The input does not start with the `NOVA` magic.
    BadMagic,
    /// The version byte is not [`VERSION`].
    UnsupportedVersion(u8),
    /// The flags byte sets bits outside [`flags::KNOWN`].
    UnknownFlags(u8),
    /// The stored CRC-32 does not match the payload.
    Checksum {
        /// The CRC-32 stored in the frame.
        expected: u32,
        /// The CRC-32 computed over the payload.
        actual: u32,
    },
    /// The payload is not a valid value encoding.
    Payload(DecodeError),
    /// The frame is structurally invalid (e.g. an overlong varint, a non-UTF-8
    /// schema reference, or a payload length that disagrees with the value).
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

/// The largest payload [`decode_frame`] accepts, in bytes (64 MiB).
///
/// Bounds the work a corrupt length can cause: the payload is checksummed
/// before it is decoded, and a recovering [`FrameReader`] retries at every
/// `NOVA` marker inside a rejected frame.
///
/// It does not bound memory. A decoded [`Value`] takes about 80 bytes per
/// element, so a full payload of 1-byte nulls decodes to about 5 GiB. Services
/// that read frames from untrusted peers should check the length first.
pub const MAX_PAYLOAD: usize = 64 << 20;

/// Slicing-by-8 tables for [`crc32`]: `CRC_TABLES[j][b]` is the CRC
/// contribution of byte `b` followed by `j` zero bytes.
const CRC_TABLES: [[u32; 256]; 8] = {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0u32;
    while i < 256 {
        let mut c = i;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            k += 1;
        }

        t[0][i as usize] = c;
        i += 1;
    }

    let mut j = 1;
    while j < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[j - 1][i];
            t[j][i] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            i += 1;
        }

        j += 1;
    }

    t
};

/// CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    !crc32_update(0xFFFF_FFFF, data)
}

#[cfg(test)]
thread_local! {
    /// Bytes fed to [`crc32_update`] on this thread, for the recovery-cost test.
    static HASHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Advance the raw (non-inverted) CRC-32 register `crc` over `data`.
fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    #[cfg(test)]
    HASHED.with(|h| h.set(h.get() + data.len()));
    let t = &CRC_TABLES;
    let (chunks, rest) = data.as_chunks::<8>();
    for c in chunks {
        let lo = crc ^ u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        crc = t[7][(lo & 0xFF) as usize]
            ^ t[6][((lo >> 8) & 0xFF) as usize]
            ^ t[5][((lo >> 16) & 0xFF) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][usize::from(c[4])]
            ^ t[2][usize::from(c[5])]
            ^ t[1][usize::from(c[6])]
            ^ t[0][usize::from(c[7])];
    }

    for &b in rest {
        crc = (crc >> 8) ^ t[0][((crc ^ u32::from(b)) & 0xFF) as usize];
    }

    crc
}

/// `a * b` modulo the CRC-32 polynomial, bit-reflected (zlib `multmodp`).
const fn crc_mul(a: u32, mut b: u32) -> u32 {
    let mut m = 1u32 << 31;
    let mut p = 0;
    while m != 0 {
        if a & m != 0 {
            p ^= b;
        }

        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ 0xEDB8_8320 } else { b >> 1 };
    }

    p
}

/// `X2N[k]` is `x^(2^k)` modulo the CRC-32 polynomial; the sequence repeats after 32.
const X2N: [u32; 32] = {
    let mut t = [0u32; 32];
    t[0] = 1 << 30; // x^1
    let mut k = 1;
    while k < 32 {
        t[k] = crc_mul(t[k - 1], t[k - 1]);
        k += 1;
    }

    t
};

/// `crc` advanced over `n` zero bytes without the init/final inversion, i.e.
/// `crc * x^(8n)`: `crc32(a ++ b) == crc_shift(crc32(a), b.len()) ^ crc32(b)`.
fn crc_shift(crc: u32, mut n: usize) -> u32 {
    let mut p = crc;
    let mut k = 3; // x^(8n) = x^(n * 2^3)
    while n != 0 {
        if n & 1 != 0 {
            p = crc_mul(X2N[k & 31], p);
        }

        n >>= 1;
        k += 1;
    }

    p
}

/// Prefix CRCs over a buffer, so any range's CRC costs O(log n) instead of O(range).
struct PrefixCrc {
    /// `marks[k]` is `crc32(&buf[..k * PrefixCrc::STEP])`.
    marks: Vec<u32>,
}

impl PrefixCrc {
    /// Checkpoint spacing; trades memory (4 bytes per `STEP`) against per-query hashing.
    const STEP: usize = 64;

    /// Hash `buf` once, recording a checkpoint every `STEP` bytes.
    fn new(buf: &[u8]) -> Self {
        let mut marks = Vec::with_capacity(buf.len() / Self::STEP + 1);
        let mut c = 0; // crc32 of the empty prefix
        marks.push(c);
        for chunk in buf.as_chunks::<{ Self::STEP }>().0 {
            c = !crc32_update(!c, chunk);
            marks.push(c);
        }

        PrefixCrc { marks }
    }

    /// `crc32(&buf[..i])`.
    fn prefix(&self, buf: &[u8], i: usize) -> u32 {
        let k = i / Self::STEP;
        !crc32_update(!self.marks[k], &buf[k * Self::STEP..i])
    }

    /// `crc32(&buf[i..j])`.
    fn range(&self, buf: &[u8], i: usize, j: usize) -> u32 {
        self.prefix(buf, j) ^ crc_shift(self.prefix(buf, i), j - i)
    }
}

/// Append `v` as an unsigned LEB128 varint, as the value codec writes it.
fn put_varint(out: &mut Vec<u8>, v: u64) {
    novafile_value::put_varint(out, u128::from(v));
}

/// Read an unsigned LEB128 varint at `*pos`, advancing it.
///
/// Fails with [`FrameError::Truncated`] at end of input and with
/// [`FrameError::Malformed`] on values past `u64` and non-minimal encodings,
/// with the same rules as the value codec.
fn get_varint(buf: &[u8], pos: &mut usize) -> Result<u64, FrameError> {
    match novafile_value::read_varint(buf, pos) {
        Ok(v) => u64::try_from(v).map_err(|_| FrameError::Malformed("varint overflow".into())),
        Err(novafile_value::VarintError::Truncated) => Err(FrameError::Truncated),
        Err(e) => Err(FrameError::Malformed(e.to_string())),
    }
}

/// Encode one frame, setting the flags to match `frame` and `opts`.
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

    if frame.meta {
        f |= flags::META;
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
    decode_frame_with(buf, |r| crc32(&buf[r]))
}

/// [`decode_frame`] with the payload CRC taken from `crc`, given the payload's range in `buf`.
fn decode_frame_with(buf: &[u8], crc: impl FnOnce(std::ops::Range<usize>) -> u32) -> Result<(Frame, usize), FrameError> {
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
    if len > MAX_PAYLOAD {
        return Err(FrameError::Malformed(format!("payload length {len} exceeds {MAX_PAYLOAD}")));
    }

    let end = pos.checked_add(len).ok_or(FrameError::Truncated)?;
    let start = pos;
    let payload = buf.get(pos..end).ok_or(FrameError::Truncated)?;
    pos = end;
    if f & flags::CRC32 != 0 {
        let c: [u8; 4] = buf.get(pos..pos + 4).and_then(|c| c.try_into().ok()).ok_or(FrameError::Truncated)?;
        pos += 4;
        let expected = u32::from_be_bytes(c);
        let actual = crc(start..end);
        if expected != actual {
            return Err(FrameError::Checksum { expected, actual });
        }
    }

    let (value, used) = decode_prefix(payload, Limits::default()).map_err(FrameError::Payload)?;
    if used != payload.len() {
        return Err(FrameError::Malformed("payload length does not match value".into()));
    }

    Ok((
        Frame {
            schema,
            value,
            meta: f & flags::META != 0,
        },
        pos,
    ))
}

/// Iterates frames in a byte stream. With `recover`, a corrupt frame is
/// reported once and the reader resynchronizes at the next `NOVA` magic
/// (spec §5.5); without it, iteration stops at the first error.
pub struct FrameReader<'a> {
    /// The whole stream.
    buf: &'a [u8],
    /// Offset of the next frame.
    pos: usize,
    /// Resynchronize after a corrupt frame instead of stopping.
    recover: bool,
    /// Set after an error in strict mode; ends iteration.
    failed: bool,
    /// Built at the first error in recovery mode, so retries at `NOVA` markers inside a
    /// rejected frame don't re-hash overlapping payloads (quadratic on hostile streams).
    crcs: Option<PrefixCrc>,
}

impl<'a> FrameReader<'a> {
    /// A strict reader over `buf`: iteration ends at the first error.
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        FrameReader {
            buf,
            pos: 0,
            recover: false,
            failed: false,
            crcs: None,
        }
    }

    /// Switch to recovery mode: skip a corrupt frame and continue at the next `NOVA` magic.
    #[must_use]
    pub fn recovering(mut self) -> Self {
        self.recover = true;
        self
    }

    /// Byte offset of the next frame.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.pos
    }
}

impl Iterator for FrameReader<'_> {
    type Item = Result<Frame, (usize, FrameError)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.buf.len() {
            return None;
        }

        let at = self.pos;
        let buf = self.buf;
        let decoded = match &self.crcs {
            Some(p) => decode_frame_with(&buf[at..], |r| p.range(buf, at + r.start, at + r.end)),
            None => decode_frame(&buf[at..]),
        };

        match decoded {
            Ok((f, n)) => {
                self.pos += n;
                Some(Ok(f))
            }
            Err(e) => {
                if self.recover {
                    if self.crcs.is_none() {
                        self.crcs = Some(PrefixCrc::new(buf));
                    }

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

    #[test]
    fn meta_flag_round_trips() {
        let f = Frame {
            schema: None,
            value: Value::Variant("end".into(), None),
            meta: true,
        };
        let bytes = encode_frame(&f, FrameOptions::default());
        assert_ne!(bytes[5] & flags::META, 0);
        assert_eq!(decode_frame(&bytes).unwrap().0, f);
    }

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
            meta: false,
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
    fn rejects_bad_varints() {
        // A 10-byte schema length with bit 64 set used to wrap to 0 and decode.
        let mut overflow = b"NOVA\x01\x01".to_vec();
        overflow.extend([0x80; 9]);
        overflow.extend([0x02, 0x01, 0x00]);
        assert!(matches!(decode_frame(&overflow), Err(FrameError::Malformed(_))));
        // Payload length 1 padded with a zero byte.
        assert!(matches!(decode_frame(b"NOVA\x01\x00\x81\x00\x00"), Err(FrameError::Malformed(_))));
    }

    #[test]
    fn rejects_oversized_payload_before_reading_it() {
        let mut b = b"NOVA\x01\x04".to_vec();
        put_varint(&mut b, MAX_PAYLOAD as u64 + 1);
        assert!(matches!(decode_frame(&b), Err(FrameError::Malformed(_))));
    }

    #[test]
    fn stream_recovery() {
        let mut stream = Vec::new();
        for i in 0..3 {
            stream.extend(encode_frame(
                &Frame {
                    schema: None,
                    value: val(i),
                    meta: false,
                },
                FrameOptions::default(),
            ));
        }

        let second = encode_frame(
            &Frame {
                schema: None,
                value: val(0),
                meta: false,
            },
            FrameOptions::default(),
        )
        .len();
        stream[second + 9] ^= 0xFF; // corrupt the second frame's payload
        let all: Vec<_> = FrameReader::new(&stream).recovering().collect();
        assert_eq!(all.len(), 3);
        assert!(all[0].is_ok() && all[1].is_err() && all[2].is_ok());
        assert_eq!(all[2].as_ref().unwrap().value, val(2));
        let strict: Vec<_> = FrameReader::new(&stream).collect();
        assert_eq!(strict.len(), 2);
    }

    #[test]
    fn prefix_crc_ranges() {
        let buf: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13).to_le_bytes()[0]).collect();
        let p = PrefixCrc::new(&buf);
        for (i, j) in [(0, 0), (0, 1000), (3, 3), (5, 64), (64, 128), (63, 129), (1, 999), (700, 1000)] {
            assert_eq!(p.range(&buf, i, j), crc32(&buf[i..j]), "{i}..{j}");
        }
    }

    /// Bytes hashed by a recovering reader over `n` overlapping fake headers, and the stream length.
    fn hostile_recovery_cost(n: usize) -> (usize, usize) {
        // Each header claims a CRC-checked payload of `len` bytes that runs over all later
        // headers; before prefix CRCs every one of them hashed `len` bytes: O(n^2).
        let len = 9 * n;
        let mut stream = Vec::new();
        for _ in 0..n {
            stream.extend_from_slice(b"NOVA\x01\x04");
            put_varint(&mut stream, len as u64);
            assert_eq!(stream.len() % 9, 0, "headers must be 9 bytes");
        }

        stream.resize(stream.len() + len + 4, 0);
        HASHED.with(|h| h.set(0));
        let all: Vec<_> = FrameReader::new(&stream).recovering().collect();
        assert_eq!(all.len(), n);
        assert!(all.iter().all(|r| matches!(r, Err((_, FrameError::Checksum { .. })))));
        (HASHED.with(std::cell::Cell::get), stream.len())
    }

    #[test]
    fn recovery_is_linear_on_overlapping_fake_headers() {
        let (small, small_len) = hostile_recovery_cost(4000);
        let (large, large_len) = hostile_recovery_cost(8000);
        // One pass over the stream plus a bounded amount per candidate.
        assert!(small <= 16 * small_len, "{small} bytes hashed for {small_len}");
        assert!(large <= 16 * large_len, "{large} bytes hashed for {large_len}");
        // Doubling the headers roughly doubles the work (quadratic would quadruple it).
        assert!(large <= 5 * small / 2, "{small} -> {large}");
    }
}
