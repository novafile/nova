//! [facet](https://facet.rs) integration for NOVA: reflection-based
//! (de)serialization to text, binary and values.
//!
//! `#[derive(Facet)]` gives a type a `SHAPE`; this crate reads values through
//! `facet_reflect::Peek` and builds them through `facet_reflect::Partial`, so
//! no per-format code is generated. The mapping and the API mirror the serde
//! binding, `nova-serde` (spec §11.3).
//!
//! ```
//! use facet::Facet;
//!
//! #[derive(Facet, Debug, PartialEq)]
//! #[repr(u8)]
//! enum Status { Active, Failed(String) }
//!
//! #[derive(Facet, Debug, PartialEq)]
//! struct Service { id: String, port: u16, status: Status }
//!
//! let s = Service { id: "api".into(), port: 3000, status: Status::Failed("oom".into()) };
//! let text = nova_facet::to_string(&s).unwrap();
//! assert_eq!(text, "id: api\nport: 3000\nstatus: !Failed(oom)\n");
//! assert_eq!(nova_facet::from_str::<Service>(&text).unwrap(), s);
//! ```
//!
//! # Mapping
//!
//! | Rust (as facet describes it) | NOVA |
//! | --- | --- |
//! | `bool`, `i8`…`i128`, `u8`…`u128`, `isize`/`usize`, `f32`/`f64`, `char`, `String`/`&str`/`Cow<str>` | bool, int, float, string (char), string |
//! | `Vec<u8>`, `[u8; N]`, `&[u8]`, `Box<[u8]>` (anything facet exposes as bytes) | bytes |
//! | `None`, `()`, unit struct | null |
//! | `Some(x)`, newtype struct (`struct S(T)`, transparent or not), `#[facet(transparent)]`, `Box`/`Arc`/`Rc` | `x` |
//! | `Vec<T>`, `VecDeque<T>`, arrays, slices | list |
//! | `HashSet<T>`, `BTreeSet<T>` | set |
//! | tuples, tuple structs | tuple |
//! | struct, map with string keys | record (field order = declaration order) |
//! | map with other keys | map (when deserializing, numeric and boolean keys also accept their string form, as in JSON objects) |
//! | unit / newtype / tuple / struct variant | `!Tag`, `!Tag(x)`, `!Tag((a, b))`, `!Tag({…})` |
//! | `Result<T, E>` | `!Ok(x)`, `!Err(e)` |
//! | [`nova_value::Timestamp`], `Duration`, `Decimal`, `Rational` | the corresponding kind |
//! | [`nova_value::Value`] | itself, losslessly |
//! | `std::time::Duration` | duration |
//! | other scalars with `Display`/`FromStr` (e.g. `IpAddr`) | string |
//!
//! The NOVA value types implement `Facet` through `nova-value`'s `facet`
//! feature, which this crate enables: the four scalar kinds as opaque types
//! with a `String` proxy (so other facet formats write their canonical text),
//! `Value` as an opaque type. This crate recognizes them by shape.
//!
//! # Deserialization
//!
//! The Rust type acts as a schema, as in `nova-serde`: a string such as
//! `2026-11-17T16:00:00Z` deserializes into a [`nova_value::Timestamp`]
//! field, a string naming a unit variant into an enum, a JSON-style
//! single-entry record `{Tag: payload}` into an enum, and numeric map keys
//! accept their string form. Errors carry NOVA paths
//! (`services[1].port: invalid value: integer `70000`, expected u16`).
//!
//! # Attributes
//!
//! These `#[facet(...)]` attributes are honored: `rename`, `rename_all`,
//! `alias`, `default` (field and container), `skip`, `skip_serializing`,
//! `skip_deserializing`, `skip_serializing_if`, `skip_unless_truthy`,
//! `transparent`, `deny_unknown_fields`, `flatten` (structs and maps),
//! `proxy` (field and container), `untagged`, `tag` (internally tagged),
//! `tag` + `content` (adjacently tagged), `is_numeric` and `other` (unit
//! catch-all variant).
//!
//! # Differences from `nova-serde`
//!
//! - facet requires enums to declare a `repr` (`#[repr(u8)]`, `#[repr(C)]`, …).
//! - `Vec<u8>` is bytes (serde, without `serde_bytes`, sees a sequence).
//! - Sets are native, so there is no `with::set` helper; `std::time::Duration`
//!   maps to a duration without a helper.
//! - facet 0.46 has no `Facet` impl for `std::time::SystemTime`, so it has no
//!   mapping; use [`nova_value::Timestamp`].
//! - Borrowed targets (`&str`, `&[T]`) cannot be deserialized: values are
//!   built from an owned [`nova_value::Value`]. Use `String`, `Vec<T>` or `Cow`.
//! - facet's dynamic value types (`facet_value::Value`) and metadata
//!   containers are not supported.

mod de;
mod error;
mod ser;

pub use error::Error;
pub use nova_text::Profile;

use facet::Facet;
use nova_value::Value;

/// Convert any `Facet` type into a NOVA value.
pub fn to_value<'a, T: ?Sized + Facet<'a>>(v: &T) -> Result<Value, Error> {
    ser::value(facet_reflect::Peek::new(v))
}

/// Build a `Facet` type from a NOVA value.
pub fn from_value<T: Facet<'static>>(v: Value) -> Result<T, Error> {
    de::build(v)
}

/// Serialize to human text.
pub fn to_string<'a, T: ?Sized + Facet<'a>>(v: &T) -> Result<String, Error> {
    to_string_with(v, Profile::Human)
}

/// Serialize to text in the given profile (human, compact, canonical).
pub fn to_string_with<'a, T: ?Sized + Facet<'a>>(v: &T, profile: Profile) -> Result<String, Error> {
    Ok(nova_text::to_text(&to_value(v)?, profile))
}

/// Deserialize from a NOVA text data document.
///
/// The Rust type acts as a schema: a string such as `2026-11-17T16:00:00Z`
/// deserializes into a [`nova_value::Timestamp`] field, a string naming a
/// unit variant into an enum, and so on.
pub fn from_str<T: Facet<'static>>(s: &str) -> Result<T, Error> {
    let v = nova_text::parse(s).map_err(|e| Error::new(e.to_string()))?;
    from_value(v)
}

/// Deserialize after resolving the document against a NOVA schema (spec §6.3).
pub fn from_str_with_schema<T: Facet<'static>>(s: &str, schema: &nova_schema::Schema) -> Result<T, Error> {
    let v = nova_text::parse(s).map_err(|e| Error::new(e.to_string()))?;
    let v = schema
        .resolve(&v)
        .map_err(|issues| Error::new(issues.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("; ")))?;
    from_value(v)
}

/// Serialize to a binary frame (spec §5.4).
pub fn to_vec<'a, T: ?Sized + Facet<'a>>(v: &T) -> Result<Vec<u8>, Error> {
    let frame = nova_binary::Frame {
        schema: None,
        value: to_value(v)?,
    };
    Ok(nova_binary::encode_frame(&frame, nova_binary::FrameOptions::default()))
}

/// Deserialize from a binary frame.
pub fn from_slice<T: Facet<'static>>(b: &[u8]) -> Result<T, Error> {
    let (frame, n) = nova_binary::decode_frame(b).map_err(|e| Error::new(e.to_string()))?;
    if n != b.len() {
        return Err(Error::new("trailing bytes after frame"));
    }
    from_value(frame.value)
}
