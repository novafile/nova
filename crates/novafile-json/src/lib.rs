//! JSON adapter (spec §11.1). JSON → NOVA is lossless (integers stay exact,
//! duplicate keys are rejected). NOVA → JSON is lossless only for the JSON
//! subset; every lossy mapping is reported as a [`Warning`].

use std::fmt::Write as _;

use base64::Engine;
use novafile_value::{Decimal, Record, Value};

/// A lossy mapping performed while converting a NOVA value to JSON, or JSON
/// to NOVA with [`from_json_str_lossy`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// Where the mapping happened: `.key` and `[i]` segments from the root, empty
    /// for the root itself.
    pub path: String,
    /// What was lost or reinterpreted, e.g. "decimal written as a string".
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", if self.path.is_empty() { "." } else { &self.path }, self.message)
    }
}

/// Recursive-descent JSON parser state over a single input string.
struct P<'a> {
    /// The input as bytes, for cheap single-byte lookahead.
    b: &'a [u8],
    /// The same input as `str`, for slicing literals and multi-byte characters.
    s: &'a str,
    /// Current byte offset into the input; reported in error messages.
    i: usize,
    /// Current nesting depth of arrays and objects, capped at 128.
    depth: usize,
    /// Round numbers that no `decimal` holds to `float` (with a warning)
    /// instead of failing.
    lossy: bool,
    /// Path of the value being parsed, in [`Warning::path`] notation.
    path: String,
    /// Lossy mappings recorded so far (only with `lossy`).
    warnings: Vec<Warning>,
}

impl P<'_> {
    /// Fail with `m`, prefixed with the current byte offset.
    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("JSON byte {}: {m}", self.i))
    }

    /// Skip JSON whitespace (space, tab, LF, CR).
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    /// Consume the keyword `word` (`null`, `true`, `false`) and yield `v`.
    fn lit(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("invalid literal")
        }
    }

    /// Parse one value after optional leading whitespace.
    ///
    /// Rejects nesting beyond 128 levels and duplicate object keys.
    fn value(&mut self) -> Result<Value, String> {
        self.ws();
        self.depth += 1;
        if self.depth > 128 {
            return self.err("nesting too deep");
        }

        let v = match self.b.get(self.i) {
            None => return self.err("unexpected end of input"),
            Some(b'n') => self.lit("null", Value::Null)?,
            Some(b't') => self.lit("true", Value::Bool(true))?,
            Some(b'f') => self.lit("false", Value::Bool(false))?,
            Some(b'"') => Value::String(self.string()?),
            Some(b'[') => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        let mark = self.path.len();
                        let _ = write!(self.path, "[{}]", out.len());
                        out.push(self.value()?);
                        self.path.truncate(mark);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return self.err("expected `,` or `]`"),
                        }
                    }
                }

                Value::List(out)
            }
            Some(b'{') => {
                self.i += 1;
                let mut r = Record::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        if self.b.get(self.i) != Some(&b'"') {
                            return self.err("expected a string key");
                        }

                        let at = self.i;
                        let k = self.string()?;
                        self.ws();
                        if self.b.get(self.i) != Some(&b':') {
                            return self.err("expected `:`");
                        }

                        self.i += 1;
                        let mark = self.path.len();
                        self.path.push('.');
                        self.path.push_str(&k);
                        let v = self.value()?;
                        self.path.truncate(mark);
                        if r.insert(k.clone(), v).is_err() {
                            return Err(format!("JSON byte {at}: duplicate key {k:?}"));
                        }

                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return self.err("expected `,` or `}`"),
                        }
                    }
                }

                Value::Record(r)
            }
            Some(_) => self.number()?,
        };

        self.depth -= 1;
        Ok(v)
    }

    /// Parse a number token. Integer-looking tokens become exact `int` values
    /// (error if out of range); anything with a fraction or exponent becomes a
    /// `float` if binary64 holds it exactly, else a `decimal`. A `decimal` holds
    /// at most 38 significant digits; a longer number is an error, or with
    /// `lossy` the nearest `float` plus a [`Warning`] (still an error if it
    /// overflows or underflows binary64).
    ///
    /// The token is first scanned greedily and then validated against the strict
    /// JSON grammar (no leading zeros, no `+`, digits after `.` and `e`).
    fn number(&mut self) -> Result<Value, String> {
        let st = self.i;
        while matches!(self.b.get(self.i), Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')) {
            self.i += 1;
        }

        let t = &self.s[st..self.i];
        // JSON's number grammar is the same as NOVA's (spec §4.5).
        let intlike = t.bytes().all(|c| c.is_ascii_digit() || c == b'-');
        let valid = {
            let b = t.as_bytes();
            let mut j = usize::from(b.first() == Some(&b'-'));
            let ok_int = match b.get(j) {
                Some(b'0') => {
                    j += 1;
                    true
                }
                Some(b'1'..=b'9') => {
                    while b.get(j).is_some_and(u8::is_ascii_digit) {
                        j += 1;
                    }

                    true
                }
                _ => false,
            };

            if ok_int && b.get(j) == Some(&b'.') {
                j += 1;
                let s = j;
                while b.get(j).is_some_and(u8::is_ascii_digit) {
                    j += 1;
                }

                if s == j {
                    j = usize::MAX;
                }
            }

            if ok_int && j != usize::MAX && matches!(b.get(j), Some(b'e' | b'E')) {
                j += 1;
                if matches!(b.get(j), Some(b'+' | b'-')) {
                    j += 1;
                }

                let s = j;
                while b.get(j).is_some_and(u8::is_ascii_digit) {
                    j += 1;
                }

                if s == j {
                    j = usize::MAX;
                }
            }

            ok_int && j == b.len()
        };

        if !valid {
            self.i = st;
            return self.err("invalid number");
        }

        if intlike {
            return t.parse().map(Value::Int).map_err(|_| format!("integer {t} exceeds the supported range"));
        }

        let f: f64 = t.parse().map_err(|_| format!("invalid number {t}"))?;
        let exact = match Decimal::parse(t) {
            Ok(d) => d,
            Err(_) if self.lossy && f.is_finite() && f != 0.0 => {
                let mantissa = t.split(['e', 'E']).next().unwrap_or(t);
                let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
                let n = digits.trim_start_matches('0').trim_end_matches('0').len();
                self.warnings.push(Warning {
                    path: self.path.clone(),
                    message: format!("number with {n} significant digits rounded to float"),
                });
                return Ok(Value::Float(f));
            }
            Err(e) => return Err(format!("number {t} cannot be kept exactly: {e}")),
        };
        if f.is_finite() && Decimal::parse(&format!("{f:?}")) == Ok(exact) {
            return Ok(Value::Float(f));
        }

        Ok(Value::Decimal(exact))
    }

    /// Parse a string literal starting at its opening quote, decoding escapes.
    ///
    /// `\u` surrogate pairs are combined; a lone or malformed surrogate is an
    /// error, as are raw control characters below U+0020.
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let rest = &self.s[self.i..];
            let c = rest.chars().next().ok_or("JSON: unterminated string")?;
            self.i += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = *self.b.get(self.i).ok_or("JSON: bad escape")?;
                    self.i += 1;
                    out.push(match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with("\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return self.err("unpaired surrogate");
                                }

                                char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).ok_or("JSON: bad surrogate")?
                            } else {
                                char::from_u32(hi).ok_or("JSON: unpaired surrogate")?
                            }
                        }
                        _ => return self.err("invalid escape"),
                    });
                }
                c if (c as u32) < 0x20 => return self.err("control character in string"),
                c => out.push(c),
            }
        }
    }

    /// Read the four hex digits of a `\u` escape.
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.s.get(self.i..self.i + 4).ok_or("JSON: bad \\u escape")?;
        self.i += 4;
        // from_str_radix would also take a sign: `\u+041`.
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("JSON: bad \\u escape".into());
        }

        u32::from_str_radix(h, 16).map_err(|_| "JSON: bad \\u escape".into())
    }
}

/// Parse JSON text into a NOVA value. Numbers without fraction or exponent
/// become `int` (exact); others become `float`, or `decimal` where binary64
/// would round them. Duplicate keys are errors. This is the strict parser: a
/// number no `decimal` holds (more than 38 significant digits) is an error;
/// [`from_json_str_lossy`] rounds it to `float` instead.
///
/// # Errors
///
/// Returns a message with the byte offset if the text is not valid JSON,
/// nests too deeply, repeats an object key, has trailing content, or has a
/// number that cannot be kept exactly.
pub fn from_json_str(s: &str) -> Result<Value, String> {
    parse(s, false).map(|(v, _)| v)
}

/// Like [`from_json_str`], but a number with more than 38 significant digits
/// becomes the nearest `float` and is reported as a [`Warning`]. Callers that
/// want strict conversion treat a non-empty warning list as a failure.
///
/// # Errors
///
/// As [`from_json_str`], except for numbers that round to a finite, nonzero
/// `float`.
pub fn from_json_str_lossy(s: &str) -> Result<(Value, Vec<Warning>), String> {
    parse(s, true)
}

/// Shared body of [`from_json_str`] and [`from_json_str_lossy`].
fn parse(s: &str, lossy: bool) -> Result<(Value, Vec<Warning>), String> {
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let mut p = P {
        b: s.as_bytes(),
        s,
        i: 0,
        depth: 0,
        lossy,
        path: String::new(),
        warnings: Vec::new(),
    };

    let v = p.value()?;
    p.ws();
    if p.i != s.len() {
        return p.err("trailing content");
    }

    Ok((v, p.warnings))
}

/// Parse JSON Lines: one value per non-empty line.
///
/// # Errors
///
/// Returns the first line that fails [`from_json_str`], prefixed with its
/// 1-based line number.
pub fn from_jsonl_str(s: &str) -> Result<Vec<Value>, String> {
    s.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| from_json_str(l).map_err(|e| format!("line {}: {e}", n + 1)))
        .collect()
}

/// Like [`from_jsonl_str`], but numbers are handled as in
/// [`from_json_str_lossy`]. Warning paths start with the value's 0-based
/// index, e.g. `[2].price`, matching the list the lines form.
///
/// # Errors
///
/// Returns the first line that fails [`from_json_str_lossy`], prefixed with
/// its 1-based line number.
pub fn from_jsonl_str_lossy(s: &str) -> Result<(Vec<Value>, Vec<Warning>), String> {
    let mut warnings = Vec::new();
    let values = s
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .enumerate()
        .map(|(i, (n, l))| {
            let (v, w) = from_json_str_lossy(l).map_err(|e| format!("line {}: {e}", n + 1))?;
            warnings.extend(w.into_iter().map(|w| Warning {
                path: format!("[{i}]{}", w.path),
                ..w
            }));
            Ok(v)
        })
        .collect::<Result<_, String>>()?;
    Ok((values, warnings))
}

/// Append `s` to `out` as a JSON string literal, escaping quotes, backslashes
/// and control characters.
fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }

    out.push('"');
}

/// Serialize to JSON text (pretty with 2-space indentation, or compact),
/// reporting every lossy mapping.
#[must_use]
pub fn to_json_string(v: &Value, pretty: bool) -> (String, Vec<Warning>) {
    let mut out = String::new();
    let mut w = Vec::new();
    write(v, &mut out, &mut String::new(), &mut w, pretty.then_some(0));
    (out, w)
}

/// In pretty mode (`indent` is `Some`), start a new line indented by that many
/// spaces; in compact mode, do nothing.
fn nl(out: &mut String, indent: Option<usize>) {
    if let Some(n) = indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', n));
    }
}

/// Serialize `value` into `out`, recording a [`Warning`] at `path` for every
/// mapping JSON cannot represent exactly.
///
/// `indent` is the current indentation in pretty mode, or `None` for compact
/// output. `path` is extended while descending and restored afterwards.
#[expect(clippy::too_many_lines, reason = "flat dispatcher over every Value variant")]
fn write(value: &Value, out: &mut String, path: &mut String, warnings: &mut Vec<Warning>, indent: Option<usize>) {
    let mut warn = |m: &str| {
        warnings.push(Warning {
            path: path.clone(),
            message: m.to_owned(),
        });
    };

    let inner = indent.map(|n| n + 2);
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(&b.to_string()),
        Value::Int(i) => {
            if i.unsigned_abs() > (1u128 << 53) {
                warn("integer beyond ±2^53 may lose precision in many JSON readers");
            }

            out.push_str(&i.to_string());
        }
        Value::Float(f) if f.is_finite() => {
            let _ = write!(out, "{f:?}");
        }
        Value::Float(f) => {
            warn("non-finite float written as a string");
            quote(&f.to_string(), out);
        }
        Value::Decimal(d) => {
            warn("decimal written as a string");
            quote(&d.to_string(), out);
        }
        Value::Rational(r) => {
            warn("rational written as a \"n/d\" string");
            quote(&r.to_string(), out);
        }
        Value::String(s) => quote(s, out),
        Value::Bytes(b) => {
            warn("bytes written as a base64 string");
            quote(&base64::engine::general_purpose::STANDARD.encode(b), out);
        }
        Value::Timestamp(t) => {
            warn("timestamp written as an RFC 3339 string");
            quote(&t.to_string(), out);
        }
        Value::Duration(d) => {
            warn("duration written as a string");
            quote(&d.to_string(), out);
        }
        Value::List(l) | Value::Tuple(l) | Value::Set(l) => {
            match value {
                Value::Tuple(_) => warn("tuple written as an array"),
                Value::Set(_) => warn("set written as an array"),
                _ => {}
            }

            out.push('[');
            for (i, e) in l.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }

                nl(out, inner);
                let mark = path.len();
                let _ = write!(path, "[{i}]");
                write(e, out, path, warnings, inner);
                path.truncate(mark);
            }

            if !l.is_empty() {
                nl(out, indent);
            }

            out.push(']');
        }
        Value::Record(r) => {
            out.push('{');
            for (i, (k, e)) in r.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }

                nl(out, inner);
                quote(k, out);
                out.push_str(if indent.is_some() { ": " } else { ":" });
                let mark = path.len();
                path.push('.');
                path.push_str(k);
                write(e, out, path, warnings, inner);
                path.truncate(mark);
            }

            if !r.is_empty() {
                nl(out, indent);
            }

            out.push('}');
        }
        Value::Map(m) => {
            warn("map written as an array of [key, value] pairs");
            let pairs = Value::List(m.iter().map(|(k, v)| Value::List(vec![k.clone(), v.clone()])).collect());
            write(&pairs, out, path, warnings, indent);
        }
        Value::Variant(t, None) => {
            warn("unit variant written as a string");
            quote(t, out);
        }
        Value::Variant(t, Some(p)) => {
            warn("variant written as a single-key object");
            let mut r = Record::new();
            r.insert(t.clone(), (**p).clone()).unwrap();
            write(&Value::Record(r), out, path, warnings, indent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_escapes_take_exactly_four_hex_digits() {
        assert!(from_json_str(r#"{"a":"\u+041"}"#).is_err());
        assert!(from_json_str(r#"{"a":"\u-041"}"#).is_err());
        assert_eq!(from_json_str(r#""\u0041""#).unwrap(), Value::String("A".into()));
    }

    #[test]
    fn round_trip_json_subset() {
        let src = r#"{"a":1,"b":[true,null,"x\né🚀",1.5,-0.0],"big":123456789012345678901234567890,"c":{"d":1e3},"e":[],"f":{}}"#;
        let v = from_json_str(src).unwrap();
        let r = v.as_record().unwrap();
        assert_eq!(r.get("a"), Some(&Value::Int(1)));
        assert_eq!(r.get("big"), Some(&Value::Int(123_456_789_012_345_678_901_234_567_890)));
        assert!(matches!(r.get("c").unwrap().as_record().unwrap().get("d"), Some(Value::Float(_))));
        for pretty in [false, true] {
            let (j, w) = to_json_string(&v, pretty);
            assert_eq!(w.len(), 1, "{w:?}"); // the big int
            assert_eq!(from_json_str(&j).unwrap(), v, "{j}");
        }
    }

    #[test]
    fn numbers_binary64_would_round_stay_exact() {
        let d = |s| Value::Decimal(Decimal::parse(s).unwrap());
        assert_eq!(from_json_str("19.990000000000000000001").unwrap(), d("19.990000000000000000001"));
        assert_eq!(from_json_str("1e-400").unwrap(), d("1e-400"));
        assert_eq!(from_json_str("1e400").unwrap(), d("1e400"));
        assert_eq!(from_json_str("0.1").unwrap(), Value::Float(0.1));
        assert_eq!(from_json_str("-2.5e-3").unwrap(), Value::Float(-2.5e-3));
        assert!(from_json_str("0.1000000000000000000000000000000000000001").is_err());
    }

    #[test]
    fn numbers_beyond_decimal_round_to_float_with_a_warning() {
        let pi = "3.1415926535897932384626433832795028841971693993751";
        let src = format!(r#"{{"a":[1,{pi}],"b":19.990000000000000000001}}"#);

        // Strict: rejected.
        assert!(from_json_str(&src).unwrap_err().contains("38 significant digits"));

        // Lossy: nearest float, warned at its path; <=38 digits stay exact.
        let (v, w) = from_json_str_lossy(&src).unwrap();
        let r = v.as_record().unwrap();
        assert_eq!(r.get("a"), Some(&Value::List(vec![Value::Int(1), Value::Float(std::f64::consts::PI)])));
        assert_eq!(r.get("b"), Some(&Value::Decimal(Decimal::parse("19.990000000000000000001").unwrap())));
        assert_eq!(
            w,
            [Warning {
                path: ".a[1]".into(),
                message: "number with 50 significant digits rounded to float".into(),
            }]
        );

        // Exactly 38 significant digits is still an exact decimal, without warning.
        let d38 = format!("0.{}", "1".repeat(38));
        assert_eq!(from_json_str_lossy(&d38).unwrap(), (Value::Decimal(Decimal::parse(&d38).unwrap()), vec![]));

        // Underflow to zero is not a rounding: still an error.
        assert!(from_json_str_lossy(&format!("{pi}e-400")).is_err());

        let (l, w) = from_jsonl_str_lossy(&format!("1\n\n{{\"x\":{pi}}}\n")).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(w[0].path, "[1].x");
    }

    #[test]
    fn rejects_unpaired_surrogates() {
        for bad in [r#""\ud83dA""#, r#""\ud83d\u0041""#, r#""\ud83d""#, r#""\ude00""#] {
            assert!(from_json_str(bad).is_err(), "{bad}");
        }

        assert_eq!(from_json_str(r#""\ud83d\ude00""#).unwrap(), Value::String("\u{1f600}".into()));
    }

    #[test]
    fn rejects_bad_json() {
        assert!(from_json_str(r#"{"a":1,"a":2}"#).unwrap_err().contains("duplicate"));
        for bad in ["01", "1.", "+1", "[1,]", "{\"a\" 1}", "nul", "\"\t\"", "1 2"] {
            assert!(from_json_str(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn lossy_mappings_are_reported() {
        let v = novafile_value::Value::Variant("failed".into(), Some(Box::new(Value::Duration(novafile_value::Duration::parse("1h").unwrap()))));
        let (j, w) = to_json_string(&v, false);
        assert_eq!(j, r#"{"failed":"1h"}"#);
        assert_eq!(w.len(), 2);
    }
}
