//! `to_json`: the NOVA → JSON mapping of spec §11.1, serialized by RFC 8785 (JSON
//! Canonicalization Scheme).

use crate::ops::scalar_text;
use base64::Engine;
use novafile_value::{Value, canonical_bytes};
use std::fmt::Write as _;

/// Largest integer magnitude JSON numbers carry exactly: 2^53.
const SAFE: i128 = 1 << 53;

/// The canonical JSON text of `v`. An `int` beyond ±2^53 and a non-finite float are errors,
/// because RFC 8785 cannot represent them exactly.
pub(crate) fn to_json(v: &Value) -> Result<String, String> {
    let mut o = String::new();
    write(&mut o, v)?;
    Ok(o)
}

/// Append the JSON text of `v` to `o`.
fn write(o: &mut String, v: &Value) -> Result<(), String> {
    match v {
        Value::Null => o.push_str("null"),
        Value::Bool(b) => o.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) if i.abs() > SAFE => return Err(format!("{i} is beyond ±2^53 and has no exact JSON number")),
        Value::Int(i) => {
            let _ = write!(o, "{i}");
        }
        Value::Float(f) if !f.is_finite() => return Err("a non-finite float has no JSON number".into()),
        Value::Float(f) => o.push_str(&es_number(*f)),
        Value::String(s) => string(o, s),
        Value::Bytes(b) => string(o, &base64::engine::general_purpose::STANDARD.encode(b)),
        Value::Decimal(_) | Value::Rational(_) | Value::Timestamp(_) | Value::Duration(_) | Value::Variant(_, None) => {
            string(o, &scalar_text(v).expect("scalar"));
        }
        Value::List(l) | Value::Tuple(l) => array(o, l.iter())?,
        Value::Set(l) => {
            let mut l: Vec<&Value> = l.iter().collect();
            l.sort_by_cached_key(|v| canonical_bytes(v));
            array(o, l.into_iter())?;
        }
        Value::Map(m) => {
            let mut m: Vec<&(Value, Value)> = m.iter().collect();
            m.sort_by_cached_key(|(k, _)| canonical_bytes(k));
            o.push('[');
            for (i, (k, v)) in m.into_iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }

                o.push('[');
                write(o, k)?;
                o.push(',');
                write(o, v)?;
                o.push(']');
            }

            o.push(']');
        }
        Value::Record(r) => {
            // RFC 8785 sorts member names by their UTF-16 code units.
            let mut fields: Vec<(&String, &Value)> = r.iter().collect();
            fields.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            o.push('{');
            for (i, (k, v)) in fields.into_iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }

                string(o, k);
                o.push(':');
                write(o, v)?;
            }

            o.push('}');
        }
        Value::Variant(tag, Some(p)) => {
            o.push('{');
            string(o, tag);
            o.push(':');
            write(o, p)?;
            o.push('}');
        }
    }

    Ok(())
}

/// Append a JSON array of `items`.
fn array<'v>(o: &mut String, items: impl Iterator<Item = &'v Value>) -> Result<(), String> {
    o.push('[');
    for (i, v) in items.enumerate() {
        if i > 0 {
            o.push(',');
        }

        write(o, v)?;
    }

    o.push(']');
    Ok(())
}

/// Append a JSON string as `JSON.stringify` writes it: `"`, `\` and control characters
/// escaped, everything else literal.
fn string(o: &mut String, s: &str) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\u{8}' => o.push_str("\\b"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\u{c}' => o.push_str("\\f"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }

    o.push('"');
}

/// The ECMAScript `Number::toString` of a finite float, as RFC 8785 requires: the shortest
/// round-trip digits, positional between 1e-7 and 1e21, otherwise exponential.
#[expect(clippy::many_single_char_names, reason = "short names for operands, as in the formulas they implement")]
fn es_number(f: f64) -> String {
    if f == 0.0 {
        return "0".into();
    }

    // `{:e}` gives the shortest round-trip digits as `d.ddde±x`.
    let sci = format!("{:e}", f.abs());
    let (mant, exp) = sci.split_once('e').expect("exponent");
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let k = i64::try_from(digits.len()).expect("few digits");
    let n = exp.parse::<i64>().expect("integer exponent") + 1;
    let pad = |c: char, m: i64| c.to_string().repeat(usize::try_from(m).expect("non-negative"));
    let body = if k <= n && n <= 21 {
        digits + &pad('0', n - k)
    } else if 0 < n && n <= 21 {
        let (a, b) = digits.split_at(usize::try_from(n).expect("positive"));
        format!("{a}.{b}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", pad('0', -n))
    } else {
        let (a, b) = digits.split_at(1);
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let frac = if b.is_empty() { String::new() } else { format!(".{b}") };
        format!("{a}{frac}e{sign}{}", (n - 1).abs())
    };

    if f < 0.0 { format!("-{body}") } else { body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecmascript_numbers() {
        for (f, want) in [
            (1.0, "1"),
            (-0.0, "0"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1.5e-7, "1.5e-7"),
            (1e-6, "0.000001"),
            (123.456, "123.456"),
            (-5e-324, "-5e-324"),
            (1.797_693_134_862_315_7e308, "1.7976931348623157e+308"),
        ] {
            assert_eq!(es_number(f), want, "{f:e}");
        }
    }

    #[test]
    fn canonical_objects() {
        let v = novafile_text::parse("b: 1\na: [true, null]\n\"\\u{e9}\": x\n\"\\u{1F600}\": y\n\"\\u{FB33}\": z\n").unwrap();
        // UTF-16 order puts the surrogate pair of U+1F600 before U+FB33.
        assert_eq!(to_json(&v).unwrap(), "{\"a\":[true,null],\"b\":1,\"é\":\"x\",\"😀\":\"y\",\"\u{FB33}\":\"z\"}");
        assert!(to_json(&Value::Int(9_007_199_254_740_993)).is_err());
        assert_eq!(to_json(&Value::Int(-9_007_199_254_740_992)).unwrap(), "-9007199254740992");
        assert_eq!(to_json(&Value::String("a\"\u{1}".into())).unwrap(), "\"a\\\"\\u0001\"");
    }
}
