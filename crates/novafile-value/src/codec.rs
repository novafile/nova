//! NOVA binary value encoding (spec §5.1–5.3) and revisions.

use crate::record::INDEX_THRESHOLD;
use crate::{Decimal, DuplicateKey, Duration, Rational, Record, Timestamp, Value, ValueRef};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fmt::Write as _;

/// Type tags: the first byte of every encoded value (spec §5.1).
pub(crate) mod tag {
    /// `null`.
    pub const NULL: u8 = 0x00;
    /// Boolean `false`.
    pub const FALSE: u8 = 0x01;
    /// Boolean `true`.
    pub const TRUE: u8 = 0x02;
    /// Integer: zigzag varint.
    pub const INT: u8 = 0x03;
    /// Float: IEEE 754 binary64, big-endian; NaN is written as the canonical quiet NaN.
    pub const FLOAT: u8 = 0x04;
    /// Decimal: zigzag varint coefficient, then zigzag varint exponent.
    pub const DECIMAL: u8 = 0x05;
    /// String: string header varint (literal length or back-reference), then UTF-8 bytes.
    pub const STRING: u8 = 0x06;
    /// Bytes: varint length, then the raw bytes.
    pub const BYTES: u8 = 0x07;
    /// Timestamp: zigzag varint Unix seconds, then varint nanoseconds.
    pub const TIMESTAMP: u8 = 0x08;
    /// Duration: zigzag varint nanoseconds.
    pub const DURATION: u8 = 0x09;
    /// List: varint count, then the elements.
    pub const LIST: u8 = 0x0A;
    /// Tuple: varint count, then the elements.
    pub const TUPLE: u8 = 0x0B;
    /// Set: varint count, then the elements (sorted by canonical bytes in canonical form).
    pub const SET: u8 = 0x0C;
    /// Record: varint count, then key string and value per field.
    pub const RECORD: u8 = 0x0D;
    /// Map: varint count, then key and value per entry.
    pub const MAP: u8 = 0x0E;
    /// Unit variant: the variant name as a string.
    pub const UNIT_VARIANT: u8 = 0x0F;
    /// Variant with payload: the variant name as a string, then the payload value.
    pub const VARIANT: u8 = 0x10;
    /// Rational: zigzag varint numerator, then varint (positive) denominator.
    pub const RATIONAL: u8 = 0x11;
    // 0x12 is reserved for tensors (https://github.com/novafile/spec/blob/main/proposals/0001-tensors.md).
    /// Record list (spec §5.3): row count, field count, field names, a dense/sparse
    /// mode byte, then per row an optional presence bitmap and the present values.
    pub const RECORD_LIST: u8 = 0x13;
}

/// The single NaN bit pattern written for every NaN, so all NaNs encode (and compare) equal.
const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

/// Switches for [`encode`]; the default writes plain, non-canonical output.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeOptions {
    /// Sort records/sets/maps canonically. Implies `dedupe_strings = false`.
    pub canonical: bool,
    /// Replace repeated strings with back-references (spec §5.3).
    pub dedupe_strings: bool,
    /// Write lists of same-shaped records as record lists: field names once,
    /// then values only (spec §5.3). Ignored when `canonical`.
    pub tables: bool,
}

/// Field order for a list that can be written as a record list: at least two
/// non-empty records, every record's keys in the shared order, and at least
/// half of all cells present.
fn table_shape(items: &[Value]) -> Option<Vec<&str>> {
    if items.len() < 2 {
        return None;
    }

    let mut fields: Vec<&str> = Vec::new();
    let mut present = 0usize;
    for v in items {
        let Value::Record(r) = v else { return None };
        if r.is_empty() {
            return None;
        }

        let mut last: Option<usize> = None;
        for k in r.keys() {
            let idx = if let Some(i) = fields.iter().position(|f| f == k) {
                i
            } else {
                fields.push(k);
                fields.len() - 1
            };

            // A record whose key order disagrees with the shared order would be reordered.
            if last.is_some_and(|l| idx < l) {
                return None;
            }

            last = Some(idx);
            present += 1;
        }
    }

    (fields.len() <= 256 && present * 2 >= fields.len() * items.len()).then_some(fields)
}

/// Append `v` as an unsigned LEB128 varint.
fn put_varint(out: &mut Vec<u8>, mut v: u128) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }

        out.push(byte | 0x80);
    }
}

/// Number of bytes [`put_varint`] writes for `v`.
fn varint_len(mut v: u128) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }

    n
}

// Zigzag maps by bit reinterpretation: every i128 has exactly one u128 image.
/// Zigzag-encode a signed integer so small magnitudes get short varints.
fn zigzag(v: i128) -> u128 {
    ((v << 1) ^ (v >> 127)).cast_unsigned()
}

/// Inverse of [`zigzag`].
fn unzigzag(v: u128) -> i128 {
    (v >> 1).cast_signed() ^ -(v & 1).cast_signed()
}

/// Encoder state for one [`encode`] call over a value borrowed for `'v`.
struct Encoder<'v> {
    /// Output switches.
    opts: EncodeOptions,
    /// Strings already written as literals, mapped to their string-table index.
    /// Only populated when back-references may be emitted.
    table: HashMap<&'v str, usize>,
    /// Index the next literal string receives in the string table.
    next_index: usize,
}

impl<'v> Encoder<'v> {
    /// Write a string header and body, or a back-reference when it is shorter.
    ///
    /// Every literal advances the string-table index, matching the decoder.
    fn string(&mut self, out: &mut Vec<u8>, s: &'v str) {
        if self.opts.dedupe_strings
            && !self.opts.canonical
            && let Some(&idx) = self.table.get(s)
        {
            let r = (idx as u128) << 1 | 1;
            let lit = (s.len() as u128) << 1;
            if varint_len(r) < varint_len(lit) + s.len() {
                put_varint(out, r);
                return;
            }
        }

        put_varint(out, (s.len() as u128) << 1);
        out.extend_from_slice(s.as_bytes());
        // Every literal string enters the table; decoders mirror this. The
        // encoder only needs to track it when it may emit back-references.
        if self.opts.dedupe_strings && !self.opts.canonical {
            self.table.insert(s, self.next_index);
        }

        self.next_index += 1;
    }

    /// Canonical encodings of `items`, sorted bytewise (canonical set order).
    fn sorted_children<'a, I: Iterator<Item = &'a Value>>(items: I) -> Vec<Vec<u8>> {
        let mut parts: Vec<Vec<u8>> = items.map(canonical_bytes).collect();
        parts.sort();
        parts
    }

    /// Append the encoding of `v` to `out`.
    #[expect(clippy::too_many_lines, reason = "flat dispatcher over every Value variant")]
    fn value(&mut self, out: &mut Vec<u8>, v: &'v Value) {
        match v {
            Value::Null => out.push(tag::NULL),
            Value::Bool(false) => out.push(tag::FALSE),
            Value::Bool(true) => out.push(tag::TRUE),
            Value::Int(i) => {
                out.push(tag::INT);
                put_varint(out, zigzag(*i));
            }
            Value::Float(f) => {
                out.push(tag::FLOAT);
                let bits = if f.is_nan() { CANONICAL_NAN } else { f.to_bits() };
                out.extend_from_slice(&bits.to_be_bytes());
            }
            Value::Decimal(d) => {
                out.push(tag::DECIMAL);
                put_varint(out, zigzag(d.coefficient()));
                put_varint(out, zigzag(i128::from(d.exponent())));
            }
            Value::Rational(r) => {
                out.push(tag::RATIONAL);
                put_varint(out, zigzag(r.numerator()));
                put_varint(out, r.denominator().unsigned_abs());
            }
            Value::String(s) => {
                out.push(tag::STRING);
                self.string(out, s);
            }
            Value::Bytes(b) => {
                out.push(tag::BYTES);
                put_varint(out, b.len() as u128);
                out.extend_from_slice(b);
            }
            Value::Timestamp(t) => {
                out.push(tag::TIMESTAMP);
                put_varint(out, zigzag(i128::from(t.unix_seconds())));
                put_varint(out, u128::from(t.subsec_nanos()));
            }
            Value::Duration(d) => {
                out.push(tag::DURATION);
                put_varint(out, zigzag(d.as_nanos()));
            }
            Value::List(items) if let Some(fields) = (self.opts.tables && !self.opts.canonical).then(|| table_shape(items)).flatten() => {
                out.push(tag::RECORD_LIST);
                put_varint(out, items.len() as u128);
                put_varint(out, fields.len() as u128);
                for f in &fields {
                    self.string(out, f);
                }

                let dense = items.iter().all(|r| matches!(r, Value::Record(r) if r.len() == fields.len()));
                out.push(u8::from(!dense));
                for row in items {
                    let Value::Record(r) = row else { unreachable!() };
                    if !dense {
                        let mut bits = vec![0u8; fields.len().div_ceil(8)];
                        for (i, f) in fields.iter().enumerate() {
                            if r.contains_key(f) {
                                bits[i / 8] |= 1 << (i % 8);
                            }
                        }

                        out.extend_from_slice(&bits);
                    }

                    for f in &fields {
                        if let Some(v) = r.get(f) {
                            self.value(out, v);
                        }
                    }
                }
            }
            Value::List(items) | Value::Tuple(items) => {
                out.push(if matches!(v, Value::List(_)) { tag::LIST } else { tag::TUPLE });
                put_varint(out, items.len() as u128);
                for i in items {
                    self.value(out, i);
                }
            }
            Value::Set(items) => {
                out.push(tag::SET);
                put_varint(out, items.len() as u128);
                if self.opts.canonical {
                    for p in Self::sorted_children(items.iter()) {
                        out.extend_from_slice(&p);
                    }
                } else {
                    for i in items {
                        self.value(out, i);
                    }
                }
            }
            Value::Record(r) => {
                out.push(tag::RECORD);
                put_varint(out, r.len() as u128);
                if self.opts.canonical {
                    let mut entries: Vec<_> = r.iter().collect();
                    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
                    for (k, v) in entries {
                        self.string(out, k);
                        self.value(out, v);
                    }
                } else {
                    for (k, v) in r.iter() {
                        self.string(out, k);
                        self.value(out, v);
                    }
                }
            }
            Value::Map(entries) => {
                out.push(tag::MAP);
                put_varint(out, entries.len() as u128);
                if self.opts.canonical {
                    let mut parts: Vec<(Vec<u8>, Vec<u8>)> = entries.iter().map(|(k, v)| (canonical_bytes(k), canonical_bytes(v))).collect();
                    parts.sort();
                    for (k, v) in parts {
                        out.extend_from_slice(&k);
                        out.extend_from_slice(&v);
                    }
                } else {
                    for (k, v) in entries {
                        self.value(out, k);
                        self.value(out, v);
                    }
                }
            }
            Value::Variant(name, None) => {
                out.push(tag::UNIT_VARIANT);
                self.string(out, name);
            }
            Value::Variant(name, Some(p)) => {
                out.push(tag::VARIANT);
                self.string(out, name);
                self.value(out, p);
            }
        }
    }
}

/// Encode a value with the given options.
#[must_use]
pub fn encode(v: &Value, opts: EncodeOptions) -> Vec<u8> {
    let mut enc = Encoder {
        opts,
        table: HashMap::new(),
        next_index: 0,
    };

    let mut out = Vec::new();
    enc.value(&mut out, v);
    out
}

/// The canonical encoding (spec §5.2): the basis of equality, ordering and revisions.
#[must_use]
pub fn canonical_bytes(v: &Value) -> Vec<u8> {
    encode(
        v,
        EncodeOptions {
            canonical: true,
            dedupe_strings: false,
            tables: false,
        },
    )
}

/// Resource bounds the decoder enforces on untrusted input.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum nesting depth of containers and variant payloads.
    pub max_depth: usize,
    /// Maximum number of literal strings in the back-reference table.
    pub max_strings: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 128,
            max_strings: 1 << 20,
        }
    }
}

/// A failure to decode a binary value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    /// Byte offset in the input where the problem was detected.
    pub offset: usize,
    /// Human-readable description of the problem.
    pub message: String,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for DecodeError {}

/// Decoder state for one [`decode_prefix`] call.
struct Decoder<'a> {
    /// The whole input.
    buf: &'a [u8],
    /// Offset of the next unread byte.
    pos: usize,
    /// Current container nesting depth.
    depth: usize,
    /// Bounds checked while decoding.
    limits: Limits,
    /// Literal strings seen so far, indexed by back-references; borrowed from `buf`.
    strings: Vec<&'a str>,
}

impl<'a> Decoder<'a> {
    /// Fail with `msg` at the current offset.
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, DecodeError> {
        Err(DecodeError {
            offset: self.pos,
            message: msg.into(),
        })
    }

    /// Read one byte.
    fn byte(&mut self) -> Result<u8, DecodeError> {
        let b = *self.buf.get(self.pos).ok_or(DecodeError {
            offset: self.pos,
            message: "unexpected end of input".into(),
        })?;
        self.pos += 1;
        Ok(b)
    }

    /// Read the next `n` bytes, failing if fewer remain.
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() - self.pos < n {
            return self.err("length exceeds input");
        }

        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Read an unsigned LEB128 varint.
    ///
    /// Rejects values that overflow `u128` and non-minimal encodings (a trailing
    /// zero byte), so each integer has exactly one encoding.
    fn varint(&mut self) -> Result<u128, DecodeError> {
        let mut v: u128 = 0;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            // Fast path: most lengths, counts and small integers fit one byte.
            if shift == 0 && b & 0x80 == 0 {
                return Ok(u128::from(b));
            }

            // The 19th byte holds bits 126-127 and must be the last.
            if shift == 126 && (b & 0x7F > 0x03 || b & 0x80 != 0) {
                return self.err("varint overflow");
            }

            v |= u128::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                if b == 0 && shift > 0 {
                    return self.err("non-minimal varint");
                }

                return Ok(v);
            }

            shift += 7;
        }
    }

    /// Read an element count, rejecting counts larger than the remaining input
    /// (every element takes at least one byte).
    fn count(&mut self) -> Result<usize, DecodeError> {
        let n = self.varint()?;
        if n > (self.buf.len() - self.pos) as u128 {
            return self.err("element count exceeds remaining input");
        }

        Ok(n as usize)
    }

    /// Read a string header and resolve it: a literal is validated as UTF-8 and
    /// appended to the string table, a back-reference is looked up in it.
    fn string(&mut self) -> Result<&'a str, DecodeError> {
        let h = self.varint()?;
        if h & 1 == 1 {
            let idx = (h >> 1) as usize;
            return match self.strings.get(idx) {
                Some(s) => Ok(s),
                None => self.err(format!("string back-reference {idx} out of range")),
            };
        }

        let len = h >> 1;
        if len > (self.buf.len() - self.pos) as u128 {
            return self.err("string length exceeds input");
        }

        let start = self.pos;
        let bytes = self.take(len as usize)?;
        let s = std::str::from_utf8(bytes).map_err(|_| DecodeError {
            offset: start,
            message: "invalid UTF-8".into(),
        })?;
        if self.strings.len() >= self.limits.max_strings {
            return self.err("string table limit exceeded");
        }

        self.strings.push(s);
        Ok(s)
    }

    /// Decode one value, enforcing [`Limits`] and canonical-form invariants
    /// (normalized decimals and rationals, unique set elements, map keys and record keys).
    #[expect(clippy::too_many_lines, reason = "flat dispatcher over every wire tag")]
    fn value<T: Build<'a>>(&mut self) -> Result<T, DecodeError> {
        let start = self.pos;
        let kind = self.byte()?;
        // A record list nests twice (list, then records), as its canonical re-encoding does.
        let nesting = match kind {
            tag::LIST..=tag::MAP | tag::VARIANT => 1,
            tag::RECORD_LIST => 2,
            _ => 0,
        };
        if self.depth + nesting > self.limits.max_depth {
            return self.err("nesting depth limit exceeded");
        }

        Ok(match kind {
            tag::NULL => T::scalar(Scalar::Null),
            tag::FALSE => T::scalar(Scalar::Bool(false)),
            tag::TRUE => T::scalar(Scalar::Bool(true)),
            tag::INT => T::scalar(Scalar::Int(unzigzag(self.varint()?))),
            tag::FLOAT => {
                let b = self.take(8)?;
                T::scalar(Scalar::Float(f64::from_bits(u64::from_be_bytes(b.try_into().unwrap()))))
            }
            tag::DECIMAL => {
                let coeff = unzigzag(self.varint()?);
                let Ok(exp) = i32::try_from(unzigzag(self.varint()?)) else {
                    return self.err("decimal exponent out of range");
                };

                let d = Decimal::new(coeff, exp);
                if d.coefficient() != coeff || d.exponent() != exp {
                    return self.err("decimal is not normalized");
                }

                T::scalar(Scalar::Decimal(d))
            }
            tag::RATIONAL => {
                let num = unzigzag(self.varint()?);
                let den = match i128::try_from(self.varint()?) {
                    Ok(den) if den != 0 => den,
                    _ => return self.err("invalid rational denominator"),
                };

                match Rational::new(num, den) {
                    Ok(r) if r.numerator() == num && r.denominator() == den => T::scalar(Scalar::Rational(r)),
                    Ok(_) => return self.err("rational is not normalized"),
                    Err(e) => return self.err(e),
                }
            }
            tag::STRING => T::string(self.string()?),
            tag::BYTES => {
                let n = self.count()?;
                T::bytes(self.take(n)?)
            }
            tag::TIMESTAMP => {
                let secs = i64::try_from(unzigzag(self.varint()?));
                let nanos = u32::try_from(self.varint()?);
                let (Ok(secs), Ok(nanos)) = (secs, nanos) else {
                    return self.err("timestamp out of range");
                };

                match Timestamp::new(secs, nanos) {
                    Ok(t) => T::scalar(Scalar::Timestamp(t)),
                    Err(e) => return self.err(e),
                }
            }
            tag::DURATION => match Duration::from_nanos(unzigzag(self.varint()?)) {
                Ok(d) => T::scalar(Scalar::Duration(d)),
                Err(e) => return self.err(e),
            },
            tag::LIST | tag::TUPLE | tag::SET => {
                let n = self.count()?;
                self.depth += 1;
                let mut items = Vec::with_capacity(n);
                let mut seen = HashSet::new();
                for _ in 0..n {
                    let v = self.value::<T>()?;
                    if kind == tag::SET && !seen.insert(v.canonical()) {
                        return Err(DecodeError {
                            offset: start,
                            message: "duplicate set element".into(),
                        });
                    }

                    items.push(v);
                }

                self.depth -= 1;
                T::seq(kind, items)
            }
            tag::RECORD => {
                let n = self.count()?;
                self.depth += 1;
                let mut r = T::record(n);
                for _ in 0..n {
                    let k = self.string()?;
                    let v = self.value::<T>()?;
                    if let Err(e) = T::insert(&mut r, k, v) {
                        return Err(DecodeError {
                            offset: start,
                            message: e.to_string(),
                        });
                    }
                }

                self.depth -= 1;
                T::finish(r)
            }
            tag::MAP => {
                let n = self.count()?;
                self.depth += 1;
                let mut entries = Vec::with_capacity(n);
                let mut seen = HashSet::new();
                for _ in 0..n {
                    let k = self.value::<T>()?;
                    if !seen.insert(k.canonical()) {
                        return Err(DecodeError {
                            offset: start,
                            message: "duplicate map key".into(),
                        });
                    }

                    let v = self.value::<T>()?;
                    entries.push((k, v));
                }

                self.depth -= 1;
                T::map(entries)
            }
            tag::RECORD_LIST => {
                let n = self.count()?;
                let k = self.count()?;
                let mut fields: Vec<&'a str> = Vec::with_capacity(k);
                for _ in 0..k {
                    let f = self.string()?;
                    if fields.contains(&f) {
                        return Err(DecodeError {
                            offset: start,
                            message: format!("duplicate record-list field {f:?}"),
                        });
                    }

                    fields.push(f);
                }

                let dense = match self.byte()? {
                    0 => true,
                    1 => false,
                    m => return self.err(format!("invalid record-list mode {m}")),
                };

                self.depth += 2;
                let mut rows = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    let present: Vec<bool> = if dense {
                        vec![true; k]
                    } else {
                        let bits = self.take(k.div_ceil(8))?;
                        if k % 8 != 0 && bits[k / 8] >> (k % 8) != 0 {
                            return self.err("unused record-list presence bits must be zero");
                        }

                        (0..k).map(|i| bits[i / 8] & (1 << (i % 8)) != 0).collect()
                    };

                    // Only dense rows spend at least a byte per field, so only they bound the preallocation.
                    let mut r = T::record(if dense { k } else { 0 });
                    for (f, p) in fields.iter().zip(present) {
                        if p {
                            let v = self.value::<T>()?;
                            // Field names were checked for duplicates above.
                            T::push(&mut r, f, v);
                        }
                    }

                    rows.push(T::finish(r));
                }

                self.depth -= 2;
                T::seq(tag::LIST, rows)
            }
            tag::UNIT_VARIANT => T::variant(self.string()?, None),
            tag::VARIANT => {
                let name = self.string()?;
                self.depth += 1;
                let p = self.value::<T>()?;
                self.depth -= 1;
                T::variant(name, Some(p))
            }
            other => {
                return Err(DecodeError {
                    offset: start,
                    message: format!("unknown type tag 0x{other:02X}"),
                });
            }
        })
    }
}

/// The value kinds without heap data, as the decoder hands them to [`Build::scalar`].
pub(crate) enum Scalar {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer.
    Int(i128),
    /// A binary64 float.
    Float(f64),
    /// A normalized decimal.
    Decimal(Decimal),
    /// A normalized rational.
    Rational(Rational),
    /// A timestamp.
    Timestamp(Timestamp),
    /// A duration.
    Duration(Duration),
}

/// What the decoder builds: owned [`Value`]s or borrowed [`ValueRef`]s.
///
/// The decoder owns every check of the wire format; implementations only
/// assemble the checked parts.
pub(crate) trait Build<'a>: Sized {
    /// A record under construction.
    type Record;

    /// A value of a kind without heap data.
    fn scalar(v: Scalar) -> Self;
    /// A string borrowed from the input.
    fn string(s: &'a str) -> Self;
    /// A byte string borrowed from the input.
    fn bytes(b: &'a [u8]) -> Self;
    /// A list, tuple or set, by its wire tag.
    fn seq(kind: u8, items: Vec<Self>) -> Self;
    /// A map whose keys were checked for duplicates.
    fn map(entries: Vec<(Self, Self)>) -> Self;
    /// A variant with an optional payload.
    fn variant(name: &'a str, payload: Option<Self>) -> Self;
    /// An empty record with room for `capacity` fields.
    fn record(capacity: usize) -> Self::Record;
    /// Add a field, failing if the key is already present.
    fn insert(r: &mut Self::Record, key: &'a str, v: Self) -> Result<(), DuplicateKey>;
    /// Add a field whose key is known to be new.
    fn push(r: &mut Self::Record, key: &'a str, v: Self);
    /// The finished record.
    fn finish(r: Self::Record) -> Self;
    /// The canonical encoding, for duplicate checks in sets and maps.
    fn canonical(&self) -> Vec<u8>;
}

impl<'a> Build<'a> for Value {
    type Record = Record;

    fn scalar(v: Scalar) -> Self {
        match v {
            Scalar::Null => Value::Null,
            Scalar::Bool(b) => Value::Bool(b),
            Scalar::Int(i) => Value::Int(i),
            Scalar::Float(f) => Value::Float(f),
            Scalar::Decimal(d) => Value::Decimal(d),
            Scalar::Rational(r) => Value::Rational(r),
            Scalar::Timestamp(t) => Value::Timestamp(t),
            Scalar::Duration(d) => Value::Duration(d),
        }
    }

    fn string(s: &'a str) -> Self {
        Value::String(s.to_owned())
    }

    fn bytes(b: &'a [u8]) -> Self {
        Value::Bytes(b.to_vec())
    }

    fn seq(kind: u8, items: Vec<Self>) -> Self {
        match kind {
            tag::LIST => Value::List(items),
            tag::TUPLE => Value::Tuple(items),
            _ => Value::Set(items),
        }
    }

    fn map(entries: Vec<(Self, Self)>) -> Self {
        Value::Map(entries)
    }

    fn variant(name: &'a str, payload: Option<Self>) -> Self {
        Value::Variant(name.to_owned(), payload.map(Box::new))
    }

    fn record(capacity: usize) -> Record {
        Record::with_capacity(capacity)
    }

    fn insert(r: &mut Record, key: &'a str, v: Self) -> Result<(), DuplicateKey> {
        if r.contains_key(key) {
            return Err(DuplicateKey(key.to_owned()));
        }

        r.push(key.to_owned(), v);
        Ok(())
    }

    fn push(r: &mut Record, key: &'a str, v: Self) {
        r.push(key.to_owned(), v);
    }

    fn finish(r: Record) -> Self {
        Value::Record(r)
    }

    fn canonical(&self) -> Vec<u8> {
        canonical_bytes(self)
    }
}

/// A borrowed record under construction: its fields, and its keys once it outgrows scanning.
/// The key set is boxed so a small record under construction stays four words.
pub(crate) type RecordRef<'a> = (Vec<(&'a str, ValueRef<'a>)>, Option<Box<HashSet<&'a str>>>);

impl<'a> Build<'a> for ValueRef<'a> {
    type Record = RecordRef<'a>;

    fn scalar(v: Scalar) -> Self {
        match v {
            Scalar::Null => ValueRef::Null,
            Scalar::Bool(b) => ValueRef::Bool(b),
            Scalar::Int(i) => ValueRef::Int(i),
            Scalar::Float(f) => ValueRef::Float(f),
            Scalar::Decimal(d) => ValueRef::Decimal(d),
            Scalar::Rational(r) => ValueRef::Rational(r),
            Scalar::Timestamp(t) => ValueRef::Timestamp(t),
            Scalar::Duration(d) => ValueRef::Duration(d),
        }
    }

    fn string(s: &'a str) -> Self {
        ValueRef::String(s)
    }

    fn bytes(b: &'a [u8]) -> Self {
        ValueRef::Bytes(b)
    }

    fn seq(kind: u8, items: Vec<Self>) -> Self {
        match kind {
            tag::LIST => ValueRef::List(items),
            tag::TUPLE => ValueRef::Tuple(items),
            _ => ValueRef::Set(items),
        }
    }

    fn map(entries: Vec<(Self, Self)>) -> Self {
        ValueRef::Map(entries)
    }

    fn variant(name: &'a str, payload: Option<Self>) -> Self {
        ValueRef::Variant(name, payload.map(Box::new))
    }

    fn record(capacity: usize) -> RecordRef<'a> {
        (Vec::with_capacity(capacity), None)
    }

    fn insert((entries, keys): &mut RecordRef<'a>, key: &'a str, v: Self) -> Result<(), DuplicateKey> {
        // Small records are scanned; past that, keys go into a set (as `Record` indexes them).
        let duplicate = if entries.len() < INDEX_THRESHOLD {
            entries.iter().any(|(e, _)| *e == key)
        } else {
            !keys.get_or_insert_with(|| Box::new(entries.iter().map(|(e, _)| *e).collect())).insert(key)
        };

        if duplicate {
            return Err(DuplicateKey(key.to_owned()));
        }

        entries.push((key, v));
        Ok(())
    }

    fn push((entries, _): &mut RecordRef<'a>, key: &'a str, v: Self) {
        entries.push((key, v));
    }

    fn finish((entries, _): RecordRef<'a>) -> Self {
        ValueRef::Record(entries)
    }

    fn canonical(&self) -> Vec<u8> {
        canonical_bytes(&self.to_value())
    }
}

/// Decode one value from the start of `buf`; returns the value and bytes consumed.
///
/// # Errors
///
/// Returns a [`DecodeError`] if the input is truncated or malformed, a value
/// is out of range or not normalized, or a [`Limits`] bound is exceeded.
pub fn decode_prefix(buf: &[u8], limits: Limits) -> Result<(Value, usize), DecodeError> {
    decode_any(buf, limits)
}

/// Decode one value from the start of `buf` without copying its strings and
/// byte strings; returns the value and bytes consumed.
///
/// # Errors
///
/// As [`decode_prefix`].
pub fn decode_ref_prefix(buf: &[u8], limits: Limits) -> Result<(ValueRef<'_>, usize), DecodeError> {
    decode_any(buf, limits)
}

/// Decode one value from the start of `buf` as `T`; returns it and the bytes consumed.
fn decode_any<'a, T: Build<'a>>(buf: &'a [u8], limits: Limits) -> Result<(T, usize), DecodeError> {
    let mut d = Decoder {
        buf,
        pos: 0,
        depth: 0,
        limits,
        strings: Vec::new(),
    };

    let v = d.value()?;
    Ok((v, d.pos))
}

/// Decode exactly one value occupying all of `buf`.
///
/// # Errors
///
/// As [`decode_prefix`] with default limits; also fails if bytes remain after
/// the value.
pub fn decode(buf: &[u8]) -> Result<Value, DecodeError> {
    decode_whole(buf)
}

/// Decode exactly one value occupying all of `buf`, borrowing its strings and
/// byte strings from `buf`.
///
/// # Errors
///
/// As [`decode`].
pub fn decode_ref(buf: &[u8]) -> Result<ValueRef<'_>, DecodeError> {
    decode_whole(buf)
}

/// Decode exactly one value occupying all of `buf` as `T`.
fn decode_whole<'a, T: Build<'a>>(buf: &'a [u8]) -> Result<T, DecodeError> {
    let (v, n) = decode_any(buf, Limits::default())?;
    if n != buf.len() {
        return Err(DecodeError {
            offset: n,
            message: "trailing bytes after value".into(),
        });
    }

    Ok(v)
}

/// A content revision: SHA-256 of the canonical encoding (spec §2.13).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Revision(pub [u8; 32]);

impl Revision {
    /// The digest as 64 lowercase hex digits, without the `sha256:` prefix.
    #[must_use]
    pub fn hex(&self) -> String {
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// Does `text` (`sha256:<hex>`, possibly abbreviated to ≥ 8 hex digits)
    /// identify this revision?
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        let h = text.strip_prefix("sha256:").unwrap_or(text).to_ascii_lowercase();
        h.len() >= 8 && h.len() <= 64 && h.bytes().all(|c| c.is_ascii_hexdigit()) && self.hex().starts_with(&h)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Debug for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The [`Revision`] of `v`: SHA-256 of its canonical encoding.
#[must_use]
pub fn revision(v: &Value) -> Revision {
    Revision(Sha256::digest(canonical_bytes(v)).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(entries: Vec<(&str, Value)>) -> Value {
        Value::Record(Record::from_entries(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()).unwrap())
    }

    fn sample() -> Value {
        rec(vec![
            ("name", "auth".into()),
            ("port", Value::Int(8080)),
            ("ratio", Value::Float(0.5)),
            ("price", Value::Decimal(Decimal::parse("12.50").unwrap())),
            ("pi", Value::Rational(Rational::parse("22/7").unwrap())),
            ("at", Value::Timestamp(Timestamp::parse("2026-11-17T16:00:00Z").unwrap())),
            ("ttl", Value::Duration(Duration::parse("1h").unwrap())),
            ("blob", Value::Bytes(vec![0, 1, 2])),
            ("tags", Value::set(vec!["b".into(), "a".into()]).unwrap()),
            ("pair", Value::Tuple(vec![Value::Int(1), Value::Null])),
            ("m", Value::map(vec![(Value::Int(2), "two".into()), (Value::Int(1), "one".into())]).unwrap()),
            ("state", Value::Variant("failed".into(), Some(Box::new("boom".into())))),
            ("mode", Value::Variant("on".into(), None)),
            ("big", Value::Int(-(1i128 << 100))),
            (
                "list",
                Value::List(vec![rec(vec![("name", "auth".into())]), rec(vec![("name", "auth".into())])]),
            ),
        ])
    }

    #[test]
    fn round_trip_all_modes() {
        let v = sample();
        for opts in [
            EncodeOptions::default(),
            EncodeOptions {
                canonical: true,
                dedupe_strings: false,
                tables: false,
            },
            EncodeOptions {
                canonical: false,
                dedupe_strings: true,
                tables: false,
            },
            EncodeOptions {
                canonical: false,
                dedupe_strings: false,
                tables: true,
            },
            EncodeOptions {
                canonical: false,
                dedupe_strings: true,
                tables: true,
            },
        ] {
            let bytes = encode(&v, opts);
            assert_eq!(decode(&bytes).unwrap(), v);
        }

        let plain = encode(&v, EncodeOptions::default()).len();
        let dedup = encode(
            &v,
            EncodeOptions {
                canonical: false,
                dedupe_strings: true,
                tables: false,
            },
        )
        .len();
        assert!(dedup < plain);
    }

    #[test]
    fn equality_ignores_record_order() {
        let a = rec(vec![("a", Value::Int(1)), ("b", Value::Int(2))]);
        let b = rec(vec![("b", Value::Int(2)), ("a", Value::Int(1))]);
        assert_eq!(a, b);
        assert_eq!(revision(&a), revision(&b));
        assert_ne!(Value::List(vec![]), Value::Tuple(vec![]));
        assert_ne!(Value::Int(1), Value::Float(1.0));
        assert_ne!(Value::Float(0.0), Value::Float(-0.0));
        assert_eq!(Value::Float(f64::NAN), Value::Float(-f64::NAN));
    }

    #[test]
    fn decode_ref_borrows_from_the_input() {
        let v = rec(vec![("name", "auth".into()), ("blob", Value::Bytes(vec![1, 2])), ("again", "auth".into())]);
        let bytes = encode(
            &v,
            EncodeOptions {
                canonical: false,
                dedupe_strings: true,
                tables: false,
            },
        );
        let r = decode_ref(&bytes).unwrap();
        assert_eq!(r.to_value(), v);
        let ValueRef::Record(entries) = &r else { panic!("{r:?}") };
        let inside = |p: *const u8| bytes.as_ptr_range().contains(&p);
        for (k, v) in entries {
            assert!(inside(k.as_ptr()), "{k}");
            match v {
                // The back-referenced "auth" resolves to the literal's bytes.
                ValueRef::String(s) => assert!(inside(s.as_ptr())),
                ValueRef::Bytes(b) => assert!(inside(b.as_ptr())),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn rejects_bad_input() {
        // Fuzzer find: a varint running past 19 bytes overflowed the shift.
        let mut long = vec![0x11, 0x0A];
        long.extend([0x82; 20]);
        long.extend([0x00, 0xCC]);
        assert!(decode(&long).is_err());
        assert!(decode(&[tag::RECORD, 2, 2, b'a', tag::NULL, 2, b'a', tag::NULL]).is_err());
        assert!(decode(&[tag::INT, 0x80, 0x00]).is_err());
        assert!(decode(&[tag::LIST, 0x7F]).is_err());
        assert!(decode(&[0x42]).is_err());
        assert!(decode(&[tag::NULL, tag::NULL]).is_err());
    }

    #[test]
    fn revision_prefix() {
        let r = revision(&sample());
        let s = r.to_string();
        assert!(r.matches(&s));
        assert!(r.matches(&s[..15]));
        assert!(!r.matches("sha256:abc"));
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    fn rec(entries: Vec<(&str, Value)>) -> Value {
        Value::Record(Record::from_entries(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()).unwrap())
    }

    fn services(n: usize, sparse: bool) -> Value {
        Value::List(
            (0..n)
                .map(|i| {
                    let mut e = vec![
                        ("id", Value::String(format!("svc-{i}"))),
                        ("port", Value::Int(8000 + i as i128)),
                        ("replicas", Value::Int(3)),
                    ];
                    if !sparse || i % 3 == 0 {
                        e.push(("enabled", Value::Bool(true)));
                    }

                    rec(e)
                })
                .collect(),
        )
    }

    const T: EncodeOptions = EncodeOptions {
        canonical: false,
        dedupe_strings: false,
        tables: true,
    };

    #[test]
    fn record_lists_round_trip_and_shrink() {
        let td = EncodeOptions { dedupe_strings: true, ..T };
        for v in [
            services(100, false),
            services(100, true),
            rec(vec![("nested", services(5, true)), ("other", Value::List(vec![Value::Int(1)]))]),
        ] {
            for opts in [T, td] {
                assert_eq!(decode(&encode(&v, opts)).unwrap(), v);
            }
        }

        let v = services(100, false);
        let plain = encode(&v, EncodeOptions::default()).len();
        let dedupe = encode(
            &v,
            EncodeOptions {
                dedupe_strings: true,
                ..Default::default()
            },
        )
        .len();
        let table = encode(&v, T).len();
        assert!(table < dedupe && dedupe < plain, "plain {plain}, dedupe {dedupe}, table {table}");
        // Canonical encoding never uses record lists.
        assert_eq!(encode(&v, EncodeOptions { canonical: true, ..T }), canonical_bytes(&v));
    }

    #[test]
    fn record_list_shape_rules() {
        // Disagreeing key order stays a plain list (field order is preserved).
        let v = Value::List(vec![
            rec(vec![("a", Value::Int(1)), ("b", Value::Int(2))]),
            rec(vec![("b", Value::Int(2)), ("a", Value::Int(1))]),
        ]);
        assert_eq!(encode(&v, T)[0], tag::LIST);
        // Decoded field order follows the original.
        let back = decode(&encode(&services(3, true), T)).unwrap();
        let keys: Vec<_> = back.as_list().unwrap()[0].as_record().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["id", "port", "replicas", "enabled"]);
        // Malformed input is rejected.
        assert!(decode(&[tag::RECORD_LIST, 1, 2, 2, b'a', 2, b'a', 0, tag::NULL, tag::NULL]).is_err()); // duplicate field
        assert!(decode(&[tag::RECORD_LIST, 1, 1, 2, b'a', 1, 0b10, tag::NULL]).is_err()); // stray presence bit
        assert!(decode(&[tag::RECORD_LIST, 1, 1, 2, b'a', 7, tag::NULL]).is_err());
        // bad mode
    }

    /// `n` record lists, each one row holding the next in field `a`, around a null.
    fn nested_record_lists(n: usize) -> Vec<u8> {
        let mut b = vec![tag::RECORD_LIST, 1, 1, 2, b'a', 0];
        for _ in 1..n {
            b.extend([tag::RECORD_LIST, 1, 1, 1, 0]);
        }

        b.push(tag::NULL);
        b
    }

    #[test]
    fn record_lists_count_toward_depth() {
        // Record lists used to skip the depth check and overflowed the stack.
        assert!(decode(&nested_record_lists(200_000)).is_err());
        // Each record list nests twice, so 64 fill the default depth of 128 and re-encode canonically.
        let v = decode(&nested_record_lists(64)).unwrap();
        assert_eq!(decode(&canonical_bytes(&v)).unwrap(), v);
        assert!(decode(&nested_record_lists(65)).is_err());
    }
}
