//! CBOR adapter (RFC 8949), hand-written and dependency-free.
//!
//! # CBOR → NOVA
//!
//! | CBOR | NOVA |
//! | --- | --- |
//! | major 0 / 1 (unsigned / negative int) | int |
//! | byte string (definite or indefinite) | bytes |
//! | text string (definite or indefinite; UTF-8 checked) | string |
//! | array (definite or indefinite) | list |
//! | map whose keys are all text | record |
//! | any other map | map |
//! | `false`, `true`, `null` | bool, null |
//! | `undefined` (simple 23) | null |
//! | half, single, double float | float |
//! | tag 0 (RFC 3339 text) | timestamp |
//! | tag 1 (epoch seconds, int or float) | timestamp |
//! | tag 2 / 3 (bignum) | int (error beyond the signed 128-bit range) |
//! | tag 4 (decimal fraction `[exponent, mantissa]`) | decimal (exact) |
//! | tag 30 (rational `[numerator, denominator]`) | rational |
//! | tag 258 (finite set, array content) | set (duplicates are an error) |
//! | tag 259 (map datatype, map content) | map, even when every key is text |
//!
//! Every other tag, every other simple value and the reserved additional
//! information values 28–30 are errors. Duplicate map keys and set
//! elements are errors. Nesting (arrays, maps and tags) is limited to 128
//! levels, and every declared length is checked against the remaining
//! input before anything is allocated. Non-shortest encodings are
//! accepted on input.
//!
//! # NOVA → CBOR
//!
//! | NOVA | CBOR | |
//! | --- | --- | --- |
//! | null, bool | simple values 22, 20/21 | lossless |
//! | int | major 0/1, or tag 2/3 bignum beyond 64 bits | lossless |
//! | float | f64, or the shortest exact of f16/f32/f64 when deterministic | lossless |
//! | decimal | tag 4 `[exponent, mantissa]` | lossless |
//! | rational | tag 30 `[numerator, denominator]` | lossless |
//! | string, bytes | text string, byte string | lossless |
//! | timestamp | tag 0 with the canonical RFC 3339 text (nanosecond exact) | lossless |
//! | list | array | lossless |
//! | set | tag 258 array | lossless |
//! | record | map with text keys | lossless |
//! | map with a non-text key | map | lossless |
//! | map whose keys are all text (or empty) | tag 259 map (so it is not read back as a record) | lossless |
//! | duration | text string (canonical NOVA spelling) | warning |
//! | tuple | array | warning |
//! | unit variant | text string (the tag) | warning |
//! | variant with payload | single-entry map `{tag: payload}` | warning |
//!
//! Durations are *not* written with the RFC 9581 duration tag (1002): its
//! exact content format could not be verified while writing this adapter,
//! and guessing would produce data other decoders misread.
//!
//! With `deterministic`, output follows the core deterministic encoding
//! requirements of RFC 8949 §4.2.1: shortest argument encodings, no
//! indefinite lengths, floats in their shortest exact form, and map
//! entries sorted by the bytewise lexicographic order of their encoded
//! keys. Set elements are sorted the same way, so equal NOVA values always
//! produce identical bytes.

use novafile_value::{Decimal, Rational, Record, Timestamp, Value};

use crate::{Reporter, Warning, MAX_DEPTH};

const TAG_RFC3339: u64 = 0;
const TAG_EPOCH: u64 = 1;
const TAG_POS_BIGNUM: u64 = 2;
const TAG_NEG_BIGNUM: u64 = 3;
const TAG_DECIMAL: u64 = 4;
const TAG_RATIONAL: u64 = 30;
const TAG_SET: u64 = 258;
const TAG_MAP: u64 = 259;

// ---------------------------------------------------------------- decoding

/// Decode exactly one CBOR data item; trailing bytes are an error.
pub fn from_slice(b: &[u8]) -> Result<Value, String> {
    let mut d = Decoder { b, i: 0, depth: 0 };
    let v = d.item()?;
    if d.i != b.len() {
        return d.err("trailing bytes after the data item");
    }
    Ok(v)
}

struct Decoder<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl Decoder<'_> {
    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("CBOR byte {}: {m}", self.i))
    }

    fn remaining(&self) -> usize {
        self.b.len() - self.i
    }

    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if n > self.remaining() {
            return self.err("unexpected end of input");
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn peek(&self) -> Result<u8, String> {
        self.b
            .get(self.i)
            .copied()
            .ok_or_else(|| format!("CBOR byte {}: unexpected end of input", self.i))
    }

    /// The argument for additional information `ai`; `None` means
    /// indefinite length.
    fn arg(&mut self, ai: u8) -> Result<Option<u64>, String> {
        let n = match ai {
            0..=23 => return Ok(Some(u64::from(ai))),
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            31 => return Ok(None),
            _ => return self.err("reserved additional information value"),
        };
        Ok(Some(self.take(n)?.iter().fold(0u64, |a, &b| a << 8 | u64::from(b))))
    }

    fn definite(&mut self, ai: u8) -> Result<u64, String> {
        match self.arg(ai)? {
            Some(n) => Ok(n),
            None => self.err("indefinite length is not allowed here"),
        }
    }

    /// A length that must fit in the remaining input, at `unit` bytes per
    /// element, before anything is allocated.
    fn length(&mut self, n: u64, unit: usize) -> Result<usize, String> {
        match usize::try_from(n) {
            Ok(n) if n.saturating_mul(unit) <= self.remaining() => Ok(n),
            _ => self.err("declared length exceeds the remaining input"),
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err(&format!("nesting deeper than {MAX_DEPTH} levels"));
        }
        Ok(())
    }

    /// The payload of a byte (major 2) or text (major 3) string.
    fn string_payload(&mut self, major: u8, ai: u8) -> Result<Vec<u8>, String> {
        let check = |d: &Self, chunk: &[u8]| {
            if major == 3 && std::str::from_utf8(chunk).is_err() {
                return d.err("invalid UTF-8 in text string");
            }
            Ok(())
        };
        match self.arg(ai)? {
            Some(n) => {
                let n = self.length(n, 1)?;
                let s = self.take(n)?.to_vec();
                check(self, &s)?;
                Ok(s)
            }
            None => {
                let mut out = Vec::new();
                loop {
                    let ib = self.byte()?;
                    if ib == 0xff {
                        return Ok(out);
                    }
                    if ib >> 5 != major {
                        return self.err("indefinite-length string chunk of the wrong type");
                    }
                    let n = self.definite(ib & 31)?;
                    let n = self.length(n, 1)?;
                    let chunk = self.take(n)?.to_vec();
                    check(self, &chunk)?;
                    out.extend(chunk);
                }
            }
        }
    }

    /// Items of an array whose initial byte has additional info `ai`.
    fn array(&mut self, ai: u8) -> Result<Vec<Value>, String> {
        self.enter()?;
        let mut out = Vec::new();
        match self.arg(ai)? {
            Some(n) => {
                let n = self.length(n, 1)?;
                out.reserve(n);
                for _ in 0..n {
                    out.push(self.item()?);
                }
            }
            None => {
                while self.peek()? != 0xff {
                    out.push(self.item()?);
                }
                self.i += 1;
            }
        }
        self.depth -= 1;
        Ok(out)
    }

    /// Entries of a map, with duplicate keys rejected.
    fn map_entries(&mut self, ai: u8) -> Result<Vec<(Value, Value)>, String> {
        self.enter()?;
        let mut out = Vec::new();
        let mut next = |d: &mut Self| -> Result<(), String> {
            let k = d.item()?;
            let v = d.item()?;
            out.push((k, v));
            Ok(())
        };
        match self.arg(ai)? {
            Some(n) => {
                let n = self.length(n, 2)?;
                for _ in 0..n {
                    next(self)?;
                }
            }
            None => {
                while self.peek()? != 0xff {
                    next(self)?;
                }
                self.i += 1;
            }
        }
        self.depth -= 1;
        let at = self.i;
        match Value::map(out).map_err(|e| format!("CBOR byte {at}: {e}"))? {
            Value::Map(out) => Ok(out),
            _ => unreachable!("Value::map returns a map"),
        }
    }

    fn item(&mut self) -> Result<Value, String> {
        let ib = self.byte()?;
        let (major, ai) = (ib >> 5, ib & 31);
        Ok(match major {
            0 => Value::Int(i128::from(self.definite(ai)?)),
            1 => Value::Int(-1 - i128::from(self.definite(ai)?)),
            2 => Value::Bytes(self.string_payload(2, ai)?),
            3 => Value::String(String::from_utf8(self.string_payload(3, ai)?).map_err(|_| "CBOR: invalid UTF-8".to_owned())?),
            4 => Value::List(self.array(ai)?),
            5 => {
                let entries = self.map_entries(ai)?;
                if entries.iter().all(|(k, _)| matches!(k, Value::String(_))) {
                    let mut r = Record::new();
                    for (k, v) in entries {
                        let Value::String(k) = k else { unreachable!() };
                        r.insert(k, v).map_err(|e| format!("CBOR: {e}"))?;
                    }
                    Value::Record(r)
                } else {
                    Value::Map(entries)
                }
            }
            6 => {
                let tag = self.definite(ai)?;
                self.enter()?;
                let v = self.tagged(tag)?;
                self.depth -= 1;
                v
            }
            _ => match ai {
                20 => Value::Bool(false),
                21 => Value::Bool(true),
                22 | 23 => Value::Null,
                25 => Value::Float(f16_to_f64(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))),
                26 => Value::Float(f64::from(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))),
                27 => Value::Float(f64::from_be_bytes(self.take(8)?.try_into().unwrap())),
                31 => {
                    self.i -= 1;
                    return self.err("unexpected break");
                }
                28..=30 => return self.err("reserved additional information value"),
                24 => {
                    let s = self.byte()?;
                    return self.err(&format!("unsupported simple value {s}"));
                }
                _ => return self.err(&format!("unsupported simple value {ai}")),
            },
        })
    }

    fn tagged(&mut self, tag: u64) -> Result<Value, String> {
        let at = self.i;
        let bad = |m: &str| Err(format!("CBOR byte {at}: tag {tag}: {m}"));
        match tag {
            TAG_RFC3339 => match self.item()? {
                Value::String(s) => Timestamp::parse(&s).map(Value::Timestamp).or_else(|e| bad(&e)),
                _ => bad("expected a text string"),
            },
            TAG_EPOCH => match self.item()? {
                Value::Int(i) => match i64::try_from(i).map_err(|e| e.to_string()).and_then(|s| Timestamp::new(s, 0)) {
                    Ok(t) => Ok(Value::Timestamp(t)),
                    Err(e) => bad(&e),
                },
                Value::Float(f) if f.is_finite() && f.abs() < 1e17 => {
                    let mut secs = f.floor();
                    let mut nanos = ((f - secs) * 1e9).round();
                    if nanos >= 1e9 {
                        secs += 1.0;
                        nanos = 0.0;
                    }
                    Timestamp::new(secs as i64, nanos as u32).map(Value::Timestamp).or_else(|e| bad(&e))
                }
                _ => bad("expected an integer or a finite float"),
            },
            TAG_POS_BIGNUM | TAG_NEG_BIGNUM => {
                let Value::Bytes(b) = self.item()? else {
                    return bad("expected a byte string");
                };
                let digits = b.iter().skip_while(|&&x| x == 0).copied().collect::<Vec<_>>();
                if digits.len() > 16 {
                    return bad("bignum exceeds the supported integer range");
                }
                let n = digits.iter().fold(0u128, |a, &x| a << 8 | u128::from(x));
                let Ok(n) = i128::try_from(n) else {
                    return bad("bignum exceeds the supported integer range");
                };
                Ok(Value::Int(if tag == TAG_POS_BIGNUM { n } else { -1 - n }))
            }
            TAG_DECIMAL => match self.pair()? {
                (Value::Int(e), Value::Int(m)) => {
                    // Normalization moves trailing zeros of the mantissa
                    // into the exponent; that must stay within 32 bits.
                    let mut tz = 0;
                    let mut c = m;
                    while c != 0 && c % 10 == 0 {
                        c /= 10;
                        tz += 1;
                    }
                    match i32::try_from(e).ok().filter(|e| e.checked_add(tz).is_some()) {
                        Some(e) => Ok(Value::Decimal(Decimal::new(m, e))),
                        None => bad("exponent outside the 32-bit range"),
                    }
                }
                _ => bad("expected [exponent, mantissa] integers"),
            },
            TAG_RATIONAL => match self.pair()? {
                (Value::Int(n), Value::Int(d)) if d > 0 => Rational::new(n, d).map(Value::Rational).or_else(|e| bad(&e)),
                _ => bad("expected [numerator, positive denominator] integers"),
            },
            TAG_SET => match self.item()? {
                Value::List(items) => Value::set(items).or_else(|e| bad(&e)),
                _ => bad("expected an array"),
            },
            TAG_MAP => {
                let ib = self.byte()?;
                if ib >> 5 != 5 {
                    return bad("expected a map");
                }
                Ok(Value::Map(self.map_entries(ib & 31)?))
            }
            _ => self.err(&format!("unsupported tag {tag}")),
        }
    }

    /// A two-element array, as used by tags 4 and 30.
    fn pair(&mut self) -> Result<(Value, Value), String> {
        let at = self.i;
        let ib = self.byte()?;
        if ib >> 5 != 4 {
            return Err(format!("CBOR byte {at}: expected a two-element array"));
        }
        let mut items = self.array(ib & 31)?.into_iter();
        match (items.next(), items.next(), items.next()) {
            (Some(a), Some(b), None) => Ok((a, b)),
            _ => Err(format!("CBOR byte {at}: expected a two-element array")),
        }
    }
}

/// IEEE 754 binary16 → binary64 (exact).
fn f16_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let mant = f64::from(h & 0x3ff);
    sign * match exp {
        0 => mant * 2f64.powi(-24),
        31 if mant == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        _ => (1.0 + mant / 1024.0) * 2f64.powi(exp - 15),
    }
}

/// binary64 → binary16, only if the conversion is exact. NaN maps to the
/// canonical quiet NaN `0x7e00`.
fn f64_to_f16_exact(f: f64) -> Option<u16> {
    if f.is_nan() {
        return Some(0x7e00);
    }
    let bits = f.to_bits();
    let sign = ((bits >> 48) & 0x8000) as u16;
    if f.is_infinite() {
        return Some(sign | 0x7c00);
    }
    if f == 0.0 {
        return Some(sign);
    }
    let biased = ((bits >> 52) & 0x7ff) as i32;
    if biased == 0 {
        return None; // binary64 subnormals are far below binary16's range
    }
    let e = biased - 1023;
    let mant = bits & ((1 << 52) - 1);
    if (-14..=15).contains(&e) {
        // Normal binary16: 10 mantissa bits.
        if mant & ((1 << 42) - 1) != 0 {
            return None;
        }
        return Some(sign | ((e + 15) as u16) << 10 | (mant >> 42) as u16);
    }
    if (-24..-14).contains(&e) {
        // Subnormal binary16: value = m × 2^-24.
        let sig = mant | 1 << 52;
        let shift = (28 - e) as u32;
        if sig & ((1u64 << shift) - 1) != 0 {
            return None;
        }
        return Some(sign | (sig >> shift) as u16);
    }
    None
}

// ---------------------------------------------------------------- encoding

/// Encode a value as one CBOR data item, reporting every lossy mapping.
/// With `deterministic`, output is RFC 8949 §4.2.1 core deterministic.
pub fn to_vec(v: &Value, deterministic: bool) -> (Vec<u8>, Vec<Warning>) {
    let mut e = Encoder {
        det: deterministic,
        rep: Reporter::default(),
    };
    let mut out = Vec::new();
    e.value(v, &mut out);
    (out, e.rep.warnings)
}

struct Encoder {
    det: bool,
    rep: Reporter,
}

fn head(out: &mut Vec<u8>, major: u8, arg: u64) {
    let m = major << 5;
    match arg {
        0..=23 => out.push(m | arg as u8),
        24..=0xff => out.extend([m | 24, arg as u8]),
        0x100..=0xffff => {
            out.push(m | 25);
            out.extend((arg as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(m | 26);
            out.extend((arg as u32).to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend(arg.to_be_bytes());
        }
    }
}

fn text(out: &mut Vec<u8>, s: &str) {
    head(out, 3, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn int(out: &mut Vec<u8>, i: i128) {
    let (major, tag, n) = if i >= 0 {
        (0, TAG_POS_BIGNUM, i as u128)
    } else {
        (1, TAG_NEG_BIGNUM, (-1 - i) as u128)
    };
    match u64::try_from(n) {
        Ok(n) => head(out, major, n),
        Err(_) => {
            head(out, 6, tag);
            let be = n.to_be_bytes();
            let digits = &be[be.iter().position(|&b| b != 0).unwrap_or(be.len())..];
            head(out, 2, digits.len() as u64);
            out.extend_from_slice(digits);
        }
    }
}

impl Encoder {
    fn float(&self, out: &mut Vec<u8>, f: f64) {
        if !self.det {
            out.push(0xfb);
            let f = if f.is_nan() { f64::NAN } else { f };
            out.extend(f.to_bits().to_be_bytes());
        } else if let Some(h) = f64_to_f16_exact(f) {
            out.push(0xf9);
            out.extend(h.to_be_bytes());
        } else if f64::from(f as f32) == f {
            out.push(0xfa);
            out.extend((f as f32).to_bits().to_be_bytes());
        } else {
            out.push(0xfb);
            out.extend(f.to_bits().to_be_bytes());
        }
    }

    fn array(&mut self, l: &[Value], out: &mut Vec<u8>, sort: bool) {
        head(out, 4, l.len() as u64);
        let mut items: Vec<Vec<u8>> = l
            .iter()
            .enumerate()
            .map(|(i, e)| self.rep.index(i, |rep| Self::sub(self.det, rep, e)))
            .collect();
        if sort && self.det {
            items.sort();
        }
        items.into_iter().for_each(|b| out.extend(b));
    }

    /// Encode `v` into a fresh buffer with the given reporter.
    fn sub(det: bool, rep: &mut Reporter, v: &Value) -> Vec<u8> {
        let mut e = Encoder { det, rep: std::mem::take(rep) };
        let mut b = Vec::new();
        e.value(v, &mut b);
        *rep = e.rep;
        b
    }

    /// Write map entries given as already-encoded (key, value) pairs.
    fn entries(&self, mut pairs: Vec<(Vec<u8>, Vec<u8>)>, out: &mut Vec<u8>) {
        head(out, 5, pairs.len() as u64);
        if self.det {
            pairs.sort();
        }
        for (k, v) in pairs {
            out.extend(k);
            out.extend(v);
        }
    }

    fn value(&mut self, v: &Value, out: &mut Vec<u8>) {
        let det = self.det;
        match v {
            Value::Null => out.push(0xf6),
            Value::Bool(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
            Value::Int(i) => int(out, *i),
            Value::Float(f) => self.float(out, *f),
            Value::Decimal(d) => {
                head(out, 6, TAG_DECIMAL);
                head(out, 4, 2);
                int(out, i128::from(d.exponent()));
                int(out, d.coefficient());
            }
            Value::Rational(r) => {
                head(out, 6, TAG_RATIONAL);
                head(out, 4, 2);
                int(out, r.numerator());
                int(out, r.denominator());
            }
            Value::String(s) => text(out, s),
            Value::Bytes(b) => {
                head(out, 2, b.len() as u64);
                out.extend_from_slice(b);
            }
            Value::Timestamp(t) => {
                head(out, 6, TAG_RFC3339);
                text(out, &t.to_string());
            }
            Value::Duration(d) => {
                self.rep.warn("duration written as a text string");
                text(out, &d.to_string());
            }
            Value::List(l) => self.array(l, out, false),
            Value::Tuple(l) => {
                self.rep.warn("tuple written as an array");
                self.array(l, out, false);
            }
            Value::Set(l) => {
                head(out, 6, TAG_SET);
                self.array(l, out, true);
            }
            Value::Record(r) => {
                let pairs = r
                    .iter()
                    .map(|(k, e)| {
                        let mut kb = Vec::new();
                        text(&mut kb, k);
                        (kb, self.rep.field(k, |rep| Self::sub(det, rep, e)))
                    })
                    .collect();
                self.entries(pairs, out);
            }
            Value::Map(m) => {
                if m.iter().all(|(k, _)| matches!(k, Value::String(_))) {
                    head(out, 6, TAG_MAP);
                }
                let pairs = m
                    .iter()
                    .enumerate()
                    .map(|(i, (k, e))| self.rep.index(i, |rep| (Self::sub(det, rep, k), Self::sub(det, rep, e))))
                    .collect();
                self.entries(pairs, out);
            }
            Value::Variant(t, None) => {
                self.rep.warn("unit variant written as a text string");
                text(out, t);
            }
            Value::Variant(t, Some(p)) => {
                self.rep.warn("variant written as a single-entry map");
                head(out, 5, 1);
                text(out, t);
                let b = self.rep.field(t, |rep| Self::sub(det, rep, p));
                out.extend(b);
            }
        }
    }
}
