//! A schema-driven [Protocol Buffers] (proto3) adapter for NOVA values
//! (spec §11.2).
//!
//! Protobuf messages carry field numbers, not names, so the adapter needs a
//! NOVA schema (spec §6): every record field's stable `@id(n)` becomes its
//! Protobuf field number. Three operations are provided:
//!
//! - [`proto_file`] renders the proto3 `.proto` source for every definition
//!   reachable from `@root`;
//! - [`encode`] writes a value of the root type as the root message's wire bytes;
//! - [`decode`] reads such bytes back and validates the result against the schema.
//!
//! # Mapping
//!
//! | NOVA | Protobuf | Notes |
//! | --- | --- | --- |
//! | `@schema name@X.Y.Z` | `package name.vX;` | Characters other than `[A-Za-z0-9_]` in the name become `_`; anonymous schemas get no `package`. |
//! | `record R` | `message R` | Every field needs `@id(n)`: the field number (1..=536870911, not 19000..=19999). |
//! | `bool` | `bool` | |
//! | `i8` `i16` `i32` | `sint32` | |
//! | `i64` `int` | `sint64` | Encoding fails for an `int` outside the `i64` range. |
//! | `u8` `u16` `u32` | `uint32` | |
//! | `u64` | `uint64` | |
//! | `float` | `double` | |
//! | `string` | `string` | |
//! | `bytes` | `bytes` | |
//! | `decimal` | `string` | Canonical decimal text, lossless; `""` reads as `0`. |
//! | `rational` | `string` | `n/d`; `""` reads as `0/1`. |
//! | `timestamp` | `google.protobuf.Timestamp` | `{1: int64 seconds, 2: int32 nanos}`. |
//! | `duration` | `google.protobuf.Duration` | Same shape; seconds and nanos carry the same sign. |
//! | `list<T>` `T[]` `set<T>` | `repeated T` | Packed for numeric scalars and enums; both packed and unpacked input is accepted. |
//! | `map<K, V>` | `map<K, V>` | `K` must be `string` or an integer kind. |
//! | `enum E` with only unit variants | `enum E` | `E_UNSPECIFIED = 0`, then `E_VARIANT = 1, 2, …` in declared order (SCREAMING_SNAKE, prefixed by the enum name). |
//! | `enum E` with a payload variant | `message E { oneof variant { … } }` | Variant *i* (0-based) is field *i* + 1; a unit variant is a `google.protobuf.Empty` member. |
//! | `name?: T` (optional field), `T?` (nullable), field with `= default` | `optional T` | proto3 explicit presence, for scalar and enum fields; message fields always have presence. |
//! | `@deprecated` | `[deprecated = true]` | |
//! | type aliases | — | Inlined; their constraints are checked by validation, not by Protobuf. |
//!
//! # Presence and defaults
//!
//! The encoder writes every present field (zero values included) in field
//! number order; `null` is written by omitting the field. When decoding, a
//! field that is absent from the wire becomes:
//!
//! 1. absent, if the field is optional (`name?:`);
//! 2. `null`, if its type is nullable;
//! 3. its schema default, if it has `= default`;
//! 4. otherwise the proto3 zero value: `false`, `0`, `0.0`, `""`, empty bytes,
//!    an empty list/set/map, an empty message decoded recursively (so an
//!    absent `timestamp` is the Unix epoch). Unit enums have no zero value
//!    (`0` is `UNSPECIFIED`), and a union message needs a member set, so a
//!    missing enum without a default is an error.
//!
//! Before encoding, the value is *resolved* against the schema (spec §6.3),
//! so lexical forms are accepted and defaults are written explicitly. For a
//! valid value `v`, `decode(encode(v))` equals `schema.resolve(v)`, which is
//! `v` itself when `v` already spells out its defaulted fields — with these
//! exceptions, inherent to proto3:
//!
//! - an empty list, set or map in an optional field has no bytes on the wire
//!   and decodes as an absent field;
//! - `null` in a field that is both optional and nullable decodes as absent.
//!
//! The decoder follows the Protobuf parsing rules: fields may come in any
//! order, unknown fields of every wire type are skipped, the last occurrence
//! of a singular scalar wins, occurrences of a singular message merge,
//! repeated fields accumulate, and the last `oneof` member wins. Groups (wire
//! types 3 and 4) and invalid wire types are errors. All lengths are checked
//! against the remaining input and nesting is limited to 128 messages, so
//! malformed input yields `Err`, never a panic. The decoded value is
//! validated against the schema, so constraints such as `@range` and `@len`
//! are enforced (a required string field that is missing reads as `""` and
//! then fails `@len(1..)`, for instance).
//!
//! # Unsupported
//!
//! These have no faithful proto3 form and are reported as errors naming the
//! offending definition and field: `tuple`, `any` and `null` types; lists or
//! sets of lists, sets or maps; nullable list or set elements; nullable lists,
//! sets and maps; map keys other than `string` and integers; nullable map
//! values and map values that are lists, sets or maps; nullable or
//! collection-typed variant payloads (a `oneof` member cannot be repeated);
//! a root that is not a record or an enum with payloads; and, when encoding,
//! extra fields of an `@open` record (they have no field number).
//!
//! [Protocol Buffers]: https://protobuf.dev/programming-guides/proto3/

mod decode;
mod encode;
mod gen;
mod shape;
mod wire;

use novafile_schema::{Issue, Schema};
use novafile_text::{Path, Segment};
use novafile_value::Value;

/// Maximum message nesting depth accepted by [`encode`] and [`decode`].
pub const MAX_DEPTH: usize = 128;

/// Render the proto3 `.proto` source for every definition reachable from the
/// schema's `@root`.
///
/// Fails if a reachable type has no Protobuf mapping, or a record field lacks
/// a valid, unique `@id(n)`; the message names the definition and field.
///
/// ```
/// let schema = novafile_schema::parse_schema(
///     "@schema demo@2.1.0\n@root Point\n\nrecord Point:\n  x: i32 @id(1)\n  y: i32 @id(2)\n",
/// )
/// .unwrap();
/// let proto = novafile_proto::proto_file(&schema).unwrap();
/// assert!(proto.contains("package demo.v2;"));
/// assert!(proto.contains("sint32 x = 1;"));
/// ```
pub fn proto_file(schema: &Schema) -> Result<String, String> {
    let plan = shape::Plan::new(schema)?;
    gen::render(schema, &plan)
}

/// Encode `value`, a value of the schema's `@root` type, as the wire bytes of
/// the root message.
///
/// The value is first resolved against the schema (spec §6.3); any violation
/// is an error, as is a value that has no Protobuf form (see the crate docs).
pub fn encode(value: &Value, schema: &Schema) -> Result<Vec<u8>, String> {
    encode::encode(value, schema)
}

/// Decode the wire bytes of the root message into a NOVA value, which is then
/// validated against the schema.
pub fn decode(bytes: &[u8], schema: &Schema) -> Result<Value, String> {
    decode::decode(bytes, schema)
}

/// The proto3 `package` name the generated file uses, if the schema is named.
pub fn package_name(schema: &Schema) -> Option<String> {
    gen::package(schema)
}

/// An error with the path (innermost segment first) at which it happened.
#[derive(Debug)]
pub(crate) struct Fail {
    rev_path: Vec<Segment>,
    message: String,
}

impl Fail {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Fail {
            rev_path: Vec::new(),
            message: message.into(),
        }
    }

    /// Record that the error happened below `seg`.
    pub(crate) fn at(mut self, seg: Segment) -> Self {
        self.rev_path.push(seg);
        self
    }

    pub(crate) fn render(self) -> String {
        let mut segs = self.rev_path;
        segs.reverse();
        format!("{}: {}", Path(segs), self.message)
    }
}

impl From<String> for Fail {
    fn from(message: String) -> Self {
        Fail::new(message)
    }
}

/// Format schema issues as one error message.
pub(crate) fn issues(what: &str, issues: &[Issue]) -> String {
    let list: Vec<String> = issues.iter().map(Issue::to_string).collect();
    format!("{what}: {}", list.join("; "))
}
