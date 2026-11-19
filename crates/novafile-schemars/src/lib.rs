//! JSON Schema and [`schemars`] integration for NOVA schemas (spec §6, §11.1, §11.3).
//!
//! * [`schema_for`] derives a NOVA schema from a Rust type that implements
//!   [`schemars::JsonSchema`] (usually via `#[derive(JsonSchema)]`), honoring
//!   serde attributes such as `rename`, `default` and `deny_unknown_fields`.
//! * [`from_json_schema`] converts a JSON Schema document (draft 2020-12, or
//!   draft-07 with `definitions`) to a NOVA schema, reporting every construct
//!   it had to approximate or drop as a warning.
//! * [`to_json_schema`] converts a NOVA schema to a JSON Schema (2020-12) that
//!   describes the JSON that `novafile-json` writes for resolved documents.
//!
//! ```
//! use schemars::JsonSchema;
//!
//! #[derive(JsonSchema)]
//! #[allow(dead_code)]
//! struct Service {
//!     id: String,
//!     port: u16,
//!     tags: Option<Vec<String>>,
//! }
//!
//! let schema = novafile_schemars::schema_for::<Service>("services", "1.0.0").unwrap();
//! assert_eq!(
//!     schema.to_string(),
//!     "@schema services@1.0.0\n@root Service\n\nrecord Service:\n  id: string\n  port: u16\n  tags?: list<string>?\n"
//! );
//! ```
//!
//! `novafile_value::{Timestamp, Duration, Decimal, Rational, Value}` implement
//! `JsonSchema` through `novafile-value`'s `schemars` feature, which this crate
//! enables: their schemas carry an `x-nova-kind` marker and map back to the
//! exact NOVA kinds (`Value` is `true`, i.e. `any`).
//!
//! # JSON Schema → NOVA
//!
//! | JSON Schema | NOVA |
//! | --- | --- |
//! | `$defs` / `definitions` entries, `$ref` to them | named definitions (names sanitized to `[A-Z][A-Za-z0-9_]*`) |
//! | the root schema | `@root`: the referenced definition, or a new one named after `title` (else `Root`) |
//! | `$ref: "#"` | the root definition |
//! | `type: object` with `properties` | `record`; fields not in `required` are optional (`name?:`) |
//! | `additionalProperties`/`unevaluatedProperties` absent or `false` | closed record (see below) |
//! | `additionalProperties: true` or a schema, `patternProperties` | `@open` record (extra-field schemas are dropped with a warning) |
//! | object with only `additionalProperties: S` | `map<string, S>` |
//! | object with only `patternProperties: {"^\\d+$": S}` (schemars for integer keys) | `map<int, S>` |
//! | object with neither | `map<string, any>` |
//! | `items: S` | `list<S>`; with `uniqueItems: true`, `set<S>` |
//! | `prefixItems` (draft-07: array-form `items`) closed by `items: false` / `additionalItems: false` / `maxItems` | tuple (a **warning** if `minItems` allows shorter arrays) |
//! | `prefixItems` that allows more elements | `list<any>`, **warning** |
//! | `minItems`/`maxItems`, `minProperties`/`maxProperties` on lists, sets, maps | `@len` |
//! | `type: integer` with `format` `int8` … `int64`, `uint8` … `uint64` | `i8` … `u64` |
//! | `type: integer` whose `minimum`/`maximum` are exactly a sized kind's bounds | that kind |
//! | other `type: integer` | `int`, with `@range` from the bounds |
//! | `type: number` | `float` |
//! | `minimum`, `maximum` | `@range` (bounds equal to the integer kind's own bounds are left out) |
//! | `exclusiveMinimum`, `exclusiveMaximum` | integers: inclusive `@range` ±1; floats: kept inclusive, **warning** |
//! | `minLength`/`maxLength` | `@len` on the string (code points = NOVA `scalars`) |
//! | `type: string`, `format: date-time` | `timestamp` |
//! | `type: string`, `format: duration` | `duration` (NOVA reads ISO 8601 durations) |
//! | `type: string`, `contentEncoding: base64` | `bytes` |
//! | `x-nova-kind` (`timestamp`, `duration`, `decimal`, `rational`, `bytes`, `float`, `map`) | that kind |
//! | `x-nova-kind: literal` with `const` or `enum` | a string literal type, or a union of them |
//! | `type: string`, `const: "s"` | the literal type `"s"` |
//! | `enum` of strings that are variant names | `enum` with unit variants |
//! | `enum` of other strings | a union of literal types |
//! | `x-nova-pattern: p` | `@pattern(p)` |
//! | `pattern: ^(?:q)$` or `^q$` (`q` an I-Regexp without anchors) | `@pattern(q)`, `[^\n\r]` read back as `.` |
//! | `pattern` anchored at one end or none | `@pattern` with `(.\|\n\|\r)*` on each open end |
//! | `oneOf`/`anyOf` of `{"type":"string","enum":[…]}`, `{"const":"Tag"}` and `{"type":"object","properties":{"Tag":S},"required":["Tag"],"additionalProperties":false}` (serde's externally tagged enums) | `enum` with unit and payload variants |
//! | `anyOf: [S, {"type":"null"}]`, `type: [T, "null"]`, `null` in `enum` | `S?` (nullable) |
//! | `oneOf` whose alternatives are objects sharing a required `const` tag property (internally or adjacently tagged enums) | a record: the tag as `string`, the other fields optional; **warning** |
//! | other `oneOf`/`anyOf` (untagged enums), multi-entry `allOf` | `any`, **warning** |
//! | `allOf: [S]` | `S` |
//! | `default` | field default (JSON → NOVA via `novafile-json`, then resolved against the field type); the field is written `name: T = d`, whether or not it was `required` |
//! | `deprecated: true` | `@deprecated` |
//! | `x-nova-id: n`, `x-nova-key: true` | `@id(n)`, `@key` |
//! | `true`, `{}` | `any` |
//! | `type: null`, `type: boolean`, `type: string` | `null`, `bool`, `string` |
//! | `description`, `title`, `examples`, `$comment`, `readOnly`, `writeOnly`, `$schema`, `$id` | dropped silently |
//!
//! Every other construct is dropped with a warning: non-string `const`s,
//! `pattern`s outside I-Regexp (`\d`, lookaround, flags, anchors inside),
//! other `format`s, `multipleOf`, `not`,
//! `if`/`then`/`else`, `contains`, dependencies, `minProperties`/`propertyNames`
//! on records, non-string `enum`s (the base type is kept), types like `[integer, string]`, external
//! `$ref`s (become `any`), `false` schemas, field names that are not NOVA
//! names (the field is dropped and the record becomes `@open`), invalid or
//! duplicate `x-nova-id`/`x-nova-key`, and defaults that do not resolve
//! against their type. Each warning starts with the JSON pointer of the
//! construct.
//!
//! Nested types that carry a constraint (`list<u16 @range(1..5)>`) cannot be
//! written in the schema language, so the element type is hoisted into an
//! alias definition (`type ServicePortsItem = u16 @range(1..5)`).
//!
//! **Open or closed?** JSON Schema allows unknown properties unless
//! `additionalProperties` (or `unevaluatedProperties`) says otherwise, and
//! serde ignores unknown fields unless `deny_unknown_fields` is set. NOVA
//! records are closed by default (spec §6.1), and schemars emits no
//! `additionalProperties` for ordinary structs. This crate maps an object to a
//! **closed** record unless `additionalProperties` (or `unevaluatedProperties`)
//! is `true` or a schema, or `patternProperties` is present. The NOVA schema
//! therefore validates exactly what the Rust type *serializes*, and rejects
//! typos that serde would silently ignore. Add `#[schemars(extend("additionalProperties" = true))]`
//! to a type to get an `@open` record instead.
//!
//! **Field order.** `serde_json` objects are unordered, so fields come out in
//! `required` order followed by the optional fields in alphabetical order,
//! unless an `x-nova-order` array (written by [`to_json_schema`]) gives the
//! declared order. Field order affects the canonical text and fingerprint
//! (spec §6.6), never compatibility.
//!
//! # NOVA → JSON Schema
//!
//! The JSON Schema describes the JSON that `novafile_json::to_json_string`
//! produces for a valid document, both as written and *after resolution*
//! (spec §6.3) against the NOVA schema, and what `serde_json` produces for the
//! corresponding Rust types. Fields with defaults are therefore not
//! `required`, and a string-keyed map accepts both its object form (a record
//! before resolution, and `serde_json`'s form) and the `[key, value]` pairs that
//! novafile-json writes for a resolved map. Kinds that only resolution produces
//! (decimals given as plain numbers in text, for example) are described in
//! their resolved, string form.
//!
//! | NOVA | JSON Schema |
//! | --- | --- |
//! | `any` | `true` |
//! | `null`, `bool`, `string` | `type: null`, `boolean`, `string` |
//! | `int` | `type: integer` |
//! | `i8` … `u64` | `type: integer`, `format: int8` … `uint64`, `minimum`/`maximum` |
//! | `float` | `type: [number, string]`, `pattern: ^(NaN\|-?inf)$`, `x-nova-kind: float` (novafile-json writes non-finite floats as strings) |
//! | `decimal`, `rational`, `duration` | `type: string` with a `pattern` and `x-nova-kind` |
//! | `timestamp` | `type: string`, `format: date-time`, `x-nova-kind: timestamp` |
//! | `bytes` | `type: string`, `contentEncoding: base64`, `x-nova-kind: bytes` |
//! | `list<T>` | `type: array`, `items` |
//! | `set<T>` | `type: array`, `items`, `uniqueItems: true` |
//! | `(A, B)` | `type: array`, `prefixItems`, `items: false`, `minItems` |
//! | `map<string, V>` | `anyOf: [{"type": "object", "additionalProperties": V}, array of [k, v] pairs]`, `x-nova-kind: map` (`propertyNames` for a constrained or named key type) |
//! | other `map<K, V>` | array of `[k, v]` pairs (`prefixItems: [K, V]`), `x-nova-kind: map` |
//! | `T?` | `type: [T, "null"]`, or `anyOf: [T, {"type": "null"}]` |
//! | `Name` | `$ref: "#/$defs/Name"` |
//! | `record` | `type: object`, `properties`, `required` (fields that are neither optional nor defaulted); closed: `additionalProperties: false`, `@open`: `additionalProperties: true`; `x-nova-order` when the declared field order is not alphabetical |
//! | `enum` | `oneOf` of `{"type":"string","enum":[unit tags]}` and `{"type":"object","properties":{"Tag":P},"required":["Tag"],"additionalProperties":false}` (only the former if all variants are units) |
//! | `@range` on `int`/`float` | `minimum`/`maximum` |
//! | `@len` on strings (`scalars`), lists, sets, maps, records | `minLength`/`maxLength`, `minItems`/`maxItems`, `minProperties`/`maxProperties` (maps: both of the last two) |
//! | `@pattern(p)` | `pattern` translated per RFC 9485 §5.3 (`^(?:…)$`, `.` as `[^\n\r]`, literal `^`/`$` escaped) and `x-nova-pattern: p` |
//! | `"s"` | `type: string`, `const: "s"`, `x-nova-kind: literal` (`enum: ["s", null]` when nullable) |
//! | union of literals only | `type: string`, `enum`, `x-nova-kind: literal` |
//! | `@len` in another unit, or on `bytes`; `@range` on `decimal`/`rational` | `x-nova-len` / `x-nova-range` only (not enforced by JSON Schema validators) |
//! | field default, `@deprecated` | `default` (as JSON), `deprecated: true` |
//! | `@id(n)`, `@key` | `x-nova-id: n`; `x-nova-key: true` and a note in `description` |
//! | `@schema name@version` | `x-nova-schema` and `title`: `name@version`; for a qualified name also `$id: https://{name}/{version}.schema.json` |
//! | `@root R` | `$ref: "#/$defs/R"` at the top level |
//!
//! JSON Schema cannot express `@key` uniqueness or NOVA value equality: a
//! `set` is checked with `uniqueItems`, which compares JSON values (so `1` and
//! `1.0` count as equal).

mod from_json;
mod to_json;

pub use to_json::to_json_schema;

use std::fmt;

/// Errors from building a NOVA schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The schema name does not follow spec §6.7.
    InvalidName(String),
    /// The version is not a `SemVer` 2.0.0 version.
    InvalidVersion(String),
    /// The JSON Schema document cannot be converted.
    InvalidJsonSchema(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidName(n) => write!(f, "invalid schema name {n:?}: lowercase letters, digits and -_./, starting with a letter"),
            Error::InvalidVersion(m) => write!(f, "invalid schema version: {m}"),
            Error::InvalidJsonSchema(m) => write!(f, "invalid JSON Schema: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// A NOVA schema converted from a JSON Schema, with a warning for every
/// construct that was approximated or dropped.
#[derive(Clone, Debug)]
pub struct Conversion {
    /// The converted schema.
    pub schema: novafile_schema::Schema,
    /// Each warning starts with the JSON pointer of the construct, e.g.
    /// `#/$defs/Point/properties/x: exclusiveMinimum on a float is kept inclusive`.
    pub warnings: Vec<String>,
}

/// NOVA schema for a Rust type via schemars (`#[derive(JsonSchema)]`),
/// honoring serde attributes. Warnings are discarded; use [`conversion_for`]
/// to see them.
///
/// # Errors
///
/// Fails if `name` is not a valid schema name, `version` is not `SemVer`,
/// or the generated JSON Schema cannot be converted.
pub fn schema_for<T: schemars::JsonSchema>(name: &str, version: &str) -> Result<novafile_schema::Schema, Error> {
    conversion_for::<T>(name, version).map(|c| c.schema)
}

/// Like [`schema_for`], but also returns the conversion warnings.
///
/// # Errors
///
/// As [`schema_for`].
pub fn conversion_for<T: schemars::JsonSchema>(name: &str, version: &str) -> Result<Conversion, Error> {
    let generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let json = generator.into_root_schema_for::<T>().to_value();
    from_json_schema(&json, Some(name), Some(version))
}

/// Convert a JSON Schema document (draft 2020-12; draft-07 `definitions` and
/// array-form `items` are accepted too) to a NOVA schema.
///
/// The schema name and version default to those in `x-nova-schema` (as written by
/// [`to_json_schema`]), then an `https:` `$id` whose host and path form a qualified
/// name (a trailing `/{version}.schema.json` gives the version), then the legacy
/// `$id: urn:nova:name@version`, then `title`; without any of them the schema is
/// anonymous. A name without a version gets `0.1.0` and a warning.
///
/// # Errors
///
/// [`Error::InvalidName`] or [`Error::InvalidVersion`] for a bad name or
/// version, and [`Error::InvalidJsonSchema`] if the root is `false` or not
/// an object or boolean.
pub fn from_json_schema(json: &serde_json::Value, name: Option<&str>, version: Option<&str>) -> Result<Conversion, Error> {
    from_json::convert(json, name, version)
}
