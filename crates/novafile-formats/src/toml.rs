//! TOML adapter (spec §11.2), built on the `toml` crate.
//!
//! # TOML → NOVA
//!
//! | TOML | NOVA |
//! | --- | --- |
//! | root table, table, inline table | record (duplicate keys are a TOML error) |
//! | array, array of tables | list |
//! | string | string |
//! | integer | int |
//! | float (incl. `inf`, `nan`) | float |
//! | boolean | bool |
//! | offset date-time | timestamp (the offset is applied, spec §2.6) |
//! | local date-time, local date, local time | **string** in TOML's canonical spelling (NOVA has no zone-less time kinds) |
//!
//! # NOVA → TOML
//!
//! The root must be a record. Lossy mappings are reported as [`Warning`]s;
//! values TOML cannot represent at all are errors carrying their path.
//!
//! | NOVA | TOML | |
//! | --- | --- | --- |
//! | bool, string, float, list, record | native (lists of records become arrays of tables) | lossless |
//! | int within i64 | integer | lossless |
//! | int beyond i64 | — | **error** |
//! | null | — | **error** (TOML has no null) |
//! | timestamp | offset date-time in UTC (`Z`) | lossless |
//! | decimal, rational, duration | canonical text as a string | warning |
//! | bytes | base64 string | warning |
//! | tuple, set | array | warning |
//! | map | array of `[key, value]` arrays | warning |
//! | unit variant | string (the tag) | warning |
//! | variant with payload | `{tag = payload}` table | warning |

use ::toml::value::{Datetime, Offset};
use ::toml::{Table, Value as Toml};
use base64::Engine;
use novafile_value::{Record, Timestamp, Value};

use crate::{Reporter, Warning};

/// Parse a TOML document into a record.
///
/// # Errors
///
/// Returns a message if the TOML is malformed or holds a date-time NOVA
/// cannot represent.
pub fn from_str(s: &str) -> Result<Value, String> {
    let t: Table = s.parse().map_err(|e: ::toml::de::Error| format!("TOML: {}", e.message()))?;
    table(t)
}

fn table(t: Table) -> Result<Value, String> {
    let mut r = Record::new();
    for (k, v) in t {
        let v = import(v)?;
        r.insert(k, v).map_err(|e| format!("TOML: {e}"))?;
    }
    Ok(Value::Record(r))
}

fn import(v: Toml) -> Result<Value, String> {
    Ok(match v {
        Toml::String(s) => Value::String(s),
        Toml::Integer(i) => Value::Int(i.into()),
        Toml::Float(f) => Value::Float(f),
        Toml::Boolean(b) => Value::Bool(b),
        Toml::Datetime(d) => datetime(&d)?,
        Toml::Array(a) => Value::List(a.into_iter().map(import).collect::<Result<_, _>>()?),
        Toml::Table(t) => table(t)?,
    })
}

fn datetime(d: &Datetime) -> Result<Value, String> {
    let (Some(date), Some(time), Some(offset)) = (d.date, d.time, d.offset) else {
        // Local date-times, dates and times have no instant: keep their text.
        return Ok(Value::String(d.to_string()));
    };
    let off = match offset {
        Offset::Z => "Z".to_owned(),
        Offset::Custom { minutes } => {
            let sign = if minutes < 0 { '-' } else { '+' };
            let m = minutes.unsigned_abs();
            format!("{sign}{:02}:{:02}", m / 60, m % 60)
        }
    };
    let text = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}{off}",
        date.year, date.month, date.day, time.hour, time.minute, time.second, time.nanosecond
    );
    Timestamp::parse(&text).map(Value::Timestamp).map_err(|e| format!("TOML: date-time {d}: {e}"))
}

/// Serialize a record to a TOML document, reporting every lossy mapping.
///
/// # Errors
///
/// Fails on a non-record root, on `null` and on integers beyond i64.
pub fn to_string(v: &Value) -> Result<(String, Vec<Warning>), String> {
    let Value::Record(r) = v else {
        return Err(format!("TOML: the root must be a record, not a {}", v.kind()));
    };
    let mut rep = Reporter::default();
    let t = export_record(r, &mut rep)?;
    let text = ::toml::to_string(&t).map_err(|e| format!("TOML: {e}"))?;
    Ok((text, rep.warnings))
}

fn export_record(r: &Record, rep: &mut Reporter) -> Result<Table, String> {
    let mut t = Table::new();
    for (k, e) in r.iter() {
        let v = rep.field(k, |rep| export(e, rep))?;
        t.insert(k.clone(), v);
    }
    Ok(t)
}

fn export_list(l: &[Value], rep: &mut Reporter) -> Result<Toml, String> {
    let items = l
        .iter()
        .enumerate()
        .map(|(i, e)| rep.index(i, |rep| export(e, rep)))
        .collect::<Result<_, _>>()?;
    Ok(Toml::Array(items))
}

fn fail<T>(rep: &Reporter, m: &str) -> Result<T, String> {
    Err(format!("TOML: {}: {m}", if rep.path.is_empty() { "." } else { &rep.path }))
}

fn export(v: &Value, rep: &mut Reporter) -> Result<Toml, String> {
    Ok(match v {
        Value::Null => return fail(rep, "null cannot be represented in TOML"),
        Value::Bool(b) => Toml::Boolean(*b),
        Value::Int(i) => match i64::try_from(*i) {
            Ok(i) => Toml::Integer(i),
            Err(_) => return fail(rep, &format!("integer {i} is outside TOML's 64-bit range")),
        },
        Value::Float(f) => Toml::Float(*f),
        Value::String(s) => Toml::String(s.clone()),
        Value::Timestamp(t) => Toml::Datetime(utc_datetime(t)),
        Value::Decimal(d) => {
            rep.warn("decimal written as a string");
            Toml::String(d.to_string())
        }
        Value::Rational(r) => {
            rep.warn("rational written as a \"n/d\" string");
            Toml::String(r.to_string())
        }
        Value::Duration(d) => {
            rep.warn("duration written as a string");
            Toml::String(d.to_string())
        }
        Value::Bytes(b) => {
            rep.warn("bytes written as a base64 string");
            Toml::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
        Value::List(l) => export_list(l, rep)?,
        Value::Tuple(l) => {
            rep.warn("tuple written as an array");
            export_list(l, rep)?
        }
        Value::Set(l) => {
            rep.warn("set written as an array");
            export_list(l, rep)?
        }
        Value::Record(r) => Toml::Table(export_record(r, rep)?),
        Value::Map(m) => {
            rep.warn("map written as an array of [key, value] arrays");
            let pairs: Vec<Value> = m.iter().map(|(k, v)| Value::List(vec![k.clone(), v.clone()])).collect();
            export_list(&pairs, rep)?
        }
        Value::Variant(t, None) => {
            rep.warn("unit variant written as a string");
            Toml::String(t.clone())
        }
        Value::Variant(t, Some(p)) => {
            rep.warn("variant written as a single-key table");
            let mut tbl = Table::new();
            tbl.insert(t.clone(), rep.field(t, |rep| export(p, rep))?);
            Toml::Table(tbl)
        }
    })
}

/// A timestamp as a TOML offset date-time in UTC. The canonical RFC 3339
/// text (`Z`, fraction trimmed) is valid TOML, so parsing it cannot fail.
fn utc_datetime(t: &Timestamp) -> Datetime {
    t.to_string().parse().expect("canonical timestamps are valid TOML date-times")
}
