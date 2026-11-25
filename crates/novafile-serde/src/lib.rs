//! Serde integration for NOVA.
//!
//! ```
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, Debug, PartialEq)]
//! enum Status { Active, Failed(String) }
//!
//! #[derive(Serialize, Deserialize, Debug, PartialEq)]
//! struct Service { id: String, port: u16, status: Status }
//!
//! let s = Service { id: "api".into(), port: 3000, status: Status::Failed("oom".into()) };
//! let text = novafile_serde::to_string(&s).unwrap();
//! assert_eq!(text, "id: api\nport: 3000\nstatus: !Failed(oom)\n");
//! assert_eq!(novafile_serde::from_str::<Service>(&text).unwrap(), s);
//! ```
//!
//! Mapping of the serde data model (spec §11.3):
//!
//! | serde | NOVA |
//! | --- | --- |
//! | bool, integers (≤ 128 bit), f32/f64, char, str, bytes | bool, int, float, string, string, bytes |
//! | `None` / unit / unit struct | null |
//! | `Some(x)` / newtype struct | `x` |
//! | seq | list |
//! | tuple, tuple struct | tuple |
//! | map with string keys / struct | record |
//! | map with other keys | map |
//! | enum variants (unit, newtype, tuple, struct) | variant `!Tag`, `!Tag(x)`, `!Tag((a, b))`, `!Tag({..})` |
//! | [`novafile_value::Timestamp`], `Duration`, `Decimal`, `Rational` | the corresponding kinds |
//! | [`novafile_value::Value`] | itself, losslessly |
//! | sets via [`with::set`], `std::time` via [`with::duration`] / [`with::system_time`] | set, duration, timestamp |

mod de;
mod error;
mod ser;
pub mod with;

pub use de::ValueDeserializer;
pub use error::Error;
pub use novafile_text::Profile;
pub use ser::ValueSerializer;

use novafile_value::Value;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Convert any `Serialize` type into a NOVA value.
///
/// # Errors
///
/// Fails if `v`'s `Serialize` impl fails or produces something NOVA cannot
/// represent (e.g. a `u128` beyond NOVA's range, or duplicate keys or set
/// elements).
pub fn to_value<T: ?Sized + Serialize>(v: &T) -> Result<Value, Error> {
    v.serialize(ValueSerializer)
}

/// Build a `Deserialize` type from a NOVA value.
///
/// # Errors
///
/// Fails if the value does not fit `T`: a wrong kind, an out-of-range number,
/// an unknown variant, or a missing field.
pub fn from_value<T: DeserializeOwned>(v: Value) -> Result<T, Error> {
    T::deserialize(ValueDeserializer::new(v))
}

/// Serialize to human text.
///
/// # Errors
///
/// Fails if [`to_value`] does.
pub fn to_string<T: ?Sized + Serialize>(v: &T) -> Result<String, Error> {
    to_string_with(v, Profile::Human)
}

/// Serialize to text in the given profile (human, compact, canonical).
///
/// # Errors
///
/// Fails if [`to_value`] does.
pub fn to_string_with<T: ?Sized + Serialize>(v: &T, profile: Profile) -> Result<String, Error> {
    Ok(novafile_text::to_text(&to_value(v)?, profile))
}

/// Deserialize from a NOVA text data document.
///
/// The Rust type acts as a schema: a string such as `2026-11-17T16:00:00Z`
/// deserializes into a [`novafile_value::Timestamp`] field, a string naming a
/// unit variant into an enum, and so on.
///
/// # Errors
///
/// Fails if `s` is not valid NOVA text or if [`from_value`] fails.
pub fn from_str<T: DeserializeOwned>(s: &str) -> Result<T, Error> {
    let v = novafile_text::parse(s).map_err(|e| Error::new(e.with_source(s)))?;
    from_value(v)
}

/// Deserialize after resolving the document against a NOVA schema (spec §6.3).
///
/// If the document has a `@schema` line and `schema` has a name and version,
/// the reference must name the same schema at a version it can read (spec §6.7).
/// Documents without `@schema`, and anonymous schemas, skip that check.
///
/// # Errors
///
/// Fails if `s` is not valid NOVA text, its `@schema` reference does not match
/// `schema`, it does not conform to `schema` (all issues are joined into one
/// message), or if [`from_value`] fails.
#[cfg(feature = "schema")]
pub fn from_str_with_schema<T: DeserializeOwned>(s: &str, schema: &novafile_schema::Schema) -> Result<T, Error> {
    from_value(resolved(s, schema, novafile_schema::Schema::resolve)?)
}

/// Deserialize one layer of a layered configuration after resolving it
/// partially against a NOVA schema ([`novafile_schema::Schema::resolve_partial`],
/// proposal 0010).
///
/// Every record field may be absent and record rules are not checked, but
/// types, patterns, ranges and closed records are, and so is the `@schema`
/// reference, as in [`from_str_with_schema`]. Defaults are not filled in, so
/// `T` usually has `Option` fields or `#[serde(default)]`. Check the merged
/// configuration against the schema in full afterwards.
///
/// # Errors
///
/// As [`from_str_with_schema`], with the partial checks.
#[cfg(feature = "schema")]
pub fn from_str_with_schema_partial<T: DeserializeOwned>(s: &str, schema: &novafile_schema::Schema) -> Result<T, Error> {
    from_value(resolved(s, schema, novafile_schema::Schema::resolve_partial)?)
}

/// Parse `s`, check its `@schema` reference against `schema`, and resolve its
/// value with `resolve`, for [`from_str_with_schema`] and [`from_str_with_schema_partial`].
///
/// # Errors
///
/// Fails if `s` is not valid NOVA text, its `@schema` reference does not match
/// `schema`, or `resolve` reports issues (joined into one message).
#[cfg(feature = "schema")]
fn resolved(s: &str, schema: &novafile_schema::Schema, resolve: ResolveFn) -> Result<novafile_value::Value, Error> {
    let doc = novafile_text::parse_document(s).map_err(|e| Error::new(e.with_source(s)))?;
    if let Some(r) = doc.directive("schema")
        && schema.reference().is_some()
    {
        schema.accepts_reference(r).map_err(Error::new)?;
    }

    let v = doc.value().map_err(|e| Error::new(e.with_source(s)))?;
    resolve(schema, &v).map_err(|issues| Error::new(issues.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join("; ")))
}

/// A resolution of a value against a schema: [`novafile_schema::Schema::resolve`] or its partial form.
#[cfg(feature = "schema")]
type ResolveFn = fn(&novafile_schema::Schema, &novafile_value::Value) -> Result<novafile_value::Value, Vec<novafile_schema::Issue>>;

/// Serialize to a binary frame (spec §5.4).
///
/// # Errors
///
/// Fails if [`to_value`] does.
#[cfg(feature = "binary")]
pub fn to_vec<T: ?Sized + Serialize>(v: &T) -> Result<Vec<u8>, Error> {
    let frame = novafile_binary::Frame {
        schema: None,
        value: to_value(v)?,
        meta: false,
    };

    Ok(novafile_binary::encode_frame(&frame, novafile_binary::FrameOptions::default()))
}

/// Deserialize from a binary frame.
///
/// # Errors
///
/// Fails if `b` is not a valid frame, has trailing bytes, or if
/// [`from_value`] fails.
#[cfg(feature = "binary")]
pub fn from_slice<T: DeserializeOwned>(b: &[u8]) -> Result<T, Error> {
    let (frame, n) = novafile_binary::decode_frame(b).map_err(|e| Error::new(e.to_string()))?;
    if n != b.len() {
        return Err(Error::new("trailing bytes after frame"));
    }

    from_value(frame.value)
}
