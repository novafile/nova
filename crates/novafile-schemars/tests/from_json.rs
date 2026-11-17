//! Hand-written JSON Schemas → NOVA schemas.
#![recursion_limit = "256"]

use novafile_schemars::{Conversion, Error, from_json_schema};
use serde_json::json;

fn convert(j: &serde_json::Value) -> Conversion {
    let c = from_json_schema(j, Some("test"), Some("1.0.0")).unwrap();
    // Whatever comes out must be valid schema language.
    let text = c.schema.to_string();
    let reparsed = novafile_schema::parse_schema(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(reparsed.to_string(), text);
    c
}

fn body(c: &Conversion) -> String {
    c.schema.to_string().trim_start_matches("@schema test@1.0.0\n").to_owned()
}

#[test]
#[expect(clippy::too_many_lines, reason = "one table-style test covering every mapping row")]
fn draft_2020_12_mapping_rows() {
    let c = convert(&json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Everything",
        "type": "object",
        "properties": {
            "req": {"type": "string", "minLength": 1, "maxLength": 64},
            "opt": {"type": "integer", "format": "uint16", "minimum": 0, "maximum": 65535},
            "port": {"type": "integer", "format": "uint16", "minimum": 1, "maximum": 65535},
            "small": {"type": "integer", "minimum": 0, "maximum": 255},
            "bounded": {"type": "integer", "exclusiveMinimum": 0, "exclusiveMaximum": 10},
            "big": {"type": "integer"},
            "i8": {"type": "integer", "format": "int8"},
            "i64": {"type": "integer", "format": "int64"},
            "u64": {"type": "integer", "format": "uint64", "minimum": 0},
            "ratio": {"type": "number", "minimum": 0, "maximum": 1.5},
            "flag": {"type": "boolean", "default": true},
            "nothing": {"type": "null"},
            "when": {"type": "string", "format": "date-time"},
            "ttl": {"type": "string", "format": "duration", "default": "PT1H30M"},
            "blob": {"type": "string", "contentEncoding": "base64"},
            "price": {"type": "string", "x-nova-kind": "decimal"},
            "share": {"type": "string", "x-nova-kind": "rational"},
            "names": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "unique": {"type": "array", "items": {"type": "integer"}, "uniqueItems": true},
            "pair": {"type": "array", "prefixItems": [{"type": "string"}, {"type": "number"}], "items": false, "minItems": 2},
            "dict": {"type": "object", "additionalProperties": {"$ref": "#/$defs/point"}},
            "free": {"type": "object"},
            "ids": {"type": "object", "patternProperties": {"^\\d+$": {"type": "string"}}, "additionalProperties": false},
            "color": {"type": "string", "enum": ["red", "green", "blue"], "default": "red"},
            "maybe": {"anyOf": [{"$ref": "#/$defs/point"}, {"type": "null"}]},
            "nullable": {"type": ["string", "null"]},
            "nullable_enum": {"enum": ["a", "b", null]},
            "old": {"type": "string", "deprecated": true, "description": "dropped silently"},
            "inline": {"type": "object", "properties": {"x": {"type": "integer"}}, "required": ["x"]},
            "ports": {"type": "array", "items": {"type": "integer", "minimum": 1, "maximum": 10}},
            "anything": true,
            "recursive": {"$ref": "#"},
            "shape": {"$ref": "#/$defs/Shape"},
            "single": {"allOf": [{"$ref": "#/$defs/point"}]}
        },
        "required": ["req", "port", "small", "bounded", "big", "i8", "i64", "u64", "ratio", "nothing", "when", "blob",
                     "price", "share", "names", "unique", "pair", "dict", "free", "ids", "maybe", "nullable",
                     "nullable_enum", "old", "inline", "ports", "anything", "shape", "single"],
        "$defs": {
            "point": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"], "additionalProperties": true},
            "Shape": {
                "oneOf": [
                    {"type": "string", "enum": ["empty"]},
                    {"type": "string", "const": "dot", "description": "a unit variant with docs"},
                    {"type": "object", "properties": {"circle": {"type": "number"}}, "required": ["circle"], "additionalProperties": false},
                    {"type": "object", "properties": {"rect": {"type": "object", "properties": {"w": {"type": "number"}, "h": {"type": "number"}}, "required": ["w", "h"]}},
                     "required": ["rect"], "additionalProperties": false}
                ]
            }
        }
    }));
    assert_eq!(c.warnings, Vec::<String>::new());
    assert_eq!(
        body(&c),
        "\
@root Everything

record Everything:
  req: string @len(1..64)
  port: u16 @range(1..65535)
  small: u8
  bounded: int @range(1..9)
  big: int
  i8: i8
  i64: i64
  u64: u64
  ratio: float @range(0..1.5)
  nothing: null
  when: timestamp
  blob: bytes
  price: decimal
  share: rational
  names: list<string> @len(1..)
  unique: set<int>
  pair: (string, float)
  dict: map<string, Point>
  free: map<string, any>
  ids: map<int, string>
  maybe: Point?
  nullable: string?
  nullable_enum: EverythingNullableEnum?
  old: string @deprecated
  inline: EverythingInline
  ports: list<EverythingPortsItem>
  anything: any
  shape: Shape
  single: Point
  color: EverythingColor = !red
  flag: bool = true
  opt?: u16
  recursive?: Everything
  ttl: duration = @duration(\"1h30m\")

enum EverythingColor:
  red
  green
  blue

record EverythingInline:
  x: int

enum EverythingNullableEnum:
  a
  b

type EverythingPortsItem = int @range(1..10)

record Point @open:
  x: float
  y: float

enum Shape:
  empty
  dot
  circle(float)
  rect(ShapeRect)

record ShapeRect:
  w: float
  h: float
"
    );
}

#[test]
fn draft_07_definitions_and_tuples() {
    let c = from_json_schema(
        &json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$ref": "#/definitions/my-config",
            "definitions": {
                "my-config": {
                    "type": "object",
                    "properties": {
                        "pair": {"type": "array", "items": [{"type": "integer", "format": "uint8"}, {"type": "string"}], "minItems": 2, "maxItems": 2},
                        "price": {"type": "number", "exclusiveMinimum": 0},
                        "count": {"type": "integer", "minimum": 0, "maximum": 10, "exclusiveMaximum": true},
                        "level": {"allOf": [{"$ref": "#/definitions/Level"}], "default": "high"},
                        "when": {"type": "string", "format": "date-time", "default": "2026-11-17T16:00:00+01:00"}
                    },
                    "required": ["pair", "price", "count"],
                    "additionalProperties": false
                },
                "Level": {"type": "string", "enum": ["low", "high"]}
            }
        }),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        c.schema.to_string(),
        "\
@root MyConfig

enum Level:
  low
  high

record MyConfig:
  pair: (u8, string)
  price: float @range(0..)
  count: int @range(0..9)
  level: Level = !high
  when: timestamp = @timestamp(\"2026-11-17T15:00:00Z\")
"
    );
    assert_eq!(
        c.warnings,
        ["#/definitions/my-config/properties/price: exclusive bound on a float is kept inclusive"]
    );
}

#[test]
fn warnings_for_approximated_constructs() {
    let c = convert(&json!({
        "type": "object",
        "properties": {
            "c": {"const": "fixed"},
            "pat": {"type": "string", "pattern": "^[a-z]+$", "format": "email"},
            "mult": {"type": "integer", "multipleOf": 5},
            "nums": {"enum": [1, 2, 3]},
            "untagged": {"anyOf": [{"type": "integer"}, {"type": "string"}]},
            "multi": {"type": ["integer", "string"]},
            "all": {"allOf": [{"type": "object"}, {"required": ["a"]}]},
            "never": false,
            "ext": {"$ref": "https://example.com/other.json"},
            "loose": {"type": "array", "prefixItems": [{"type": "string"}]},
            "short": {"type": "array", "prefixItems": [{"type": "string"}, {"type": "string"}], "items": false},
            "not": {"not": {"type": "string"}},
            "$weird": {"type": "string"},
            "cond": {"type": "string", "if": {"minLength": 1}, "then": {"maxLength": 2}},
            "bad_default": {"type": "integer", "default": "seven"}
        },
        "additionalProperties": {"type": "string"}
    }));
    assert_eq!(
        body(&c),
        "\
@root Root

record Root @open:
  all?: any
  bad_default?: int
  c?: string
  cond?: string
  ext?: any
  loose?: list<any>
  mult?: int
  multi?: any
  never?: any
  not?: any
  nums?: int
  pat?: string
  short?: (string, string)
  untagged?: any
"
    );
    assert_eq!(
        c.warnings,
        [
            "#: additionalProperties schema is not enforced on extra fields; the record is @open",
            "#/properties/$weird: \"$weird\" is not a valid NOVA field name; the field is dropped and the record is @open",
            "#/properties/all: allOf with several schemas (or with sibling keywords) is not supported; using any",
            "#/properties/c: const \"fixed\" is not enforced",
            "#/properties/cond: if is not supported and was dropped",
            "#/properties/cond: then is not supported and was dropped",
            "#/properties/ext: $ref \"https://example.com/other.json\" cannot be resolved (only local references are supported); using any",
            "#/properties/loose: prefixItems without a fixed length mapped to list<any>",
            "#/properties/mult: multipleOf is not enforced",
            "#/properties/multi: a value of several types (integer, string) mapped to any",
            "#/properties/never: `false` schema (matches nothing) mapped to any",
            "#/properties/not: not is not supported and was dropped",
            "#/properties/nums: enum whose values are not all valid variant names was mapped to its base type; the enum constraint is dropped",
            "#/properties/pat: format \"email\" is not enforced",
            "#/properties/pat: pattern is not enforced",
            "#/properties/short: prefixItems allows arrays shorter than 2; the NOVA tuple requires all 2 elements",
            "#/properties/untagged: anyOf that is not a serde externally tagged enum (e.g. an untagged enum) mapped to any",
            "Root.bad_default: default \"seven\" dropped: .: expected int, found string",
        ]
    );
}

#[test]
fn names_and_versions() {
    // From `$id` (as written by to_json_schema) or `title`.
    let c = from_json_schema(&json!({"$id": "urn:nova:shop.orders@2.1.0", "type": "string"}), None, None).unwrap();
    assert_eq!(c.schema.to_string(), "@schema shop.orders@2.1.0\n@root Root\n\ntype Root = string\n");
    let c = from_json_schema(&json!({"title": "shop@1.0.0-rc.1", "type": "boolean"}), None, Some("1.0.0")).unwrap();
    assert_eq!(c.schema.reference().unwrap().to_string(), "shop@1.0.0");
    // A name without a version.
    let c = from_json_schema(&json!({"type": "null"}), Some("x"), None).unwrap();
    assert_eq!(c.warnings, ["#: no schema version given; using 0.1.0"]);
    // Anonymous, and a document with only definitions has no @root.
    let c = from_json_schema(&json!({"$defs": {"A": {"type": "string"}}}), None, None).unwrap();
    assert_eq!(c.schema.to_string(), "\ntype A = string\n");
    // Errors.
    assert!(matches!(from_json_schema(&json!({}), Some("Upper"), Some("1.0.0")), Err(Error::InvalidName(_))));
    assert!(matches!(from_json_schema(&json!({}), Some("x"), Some("01.0.0")), Err(Error::InvalidVersion(_))));
    assert!(matches!(from_json_schema(&json!(false), None, None), Err(Error::InvalidJsonSchema(_))));
    assert!(matches!(from_json_schema(&json!(3), None, None), Err(Error::InvalidJsonSchema(_))));
}

#[test]
fn definition_names_are_sanitized_and_unique() {
    let c = convert(&json!({
        "$ref": "#/$defs/Nullable_Inner",
        "$defs": {
            "Nullable_Inner": {"type": ["object", "null"], "properties": {"a": {"$ref": "#/$defs/my%20type"}}},
            "my type": {"type": "string"},
            "My-type": {"type": "integer"},
            "1st": {"type": "boolean"}
        }
    }));
    assert_eq!(
        body(&c),
        "\
@root Nullable_Inner

type MyType = int

type MyType2 = string

type Nullable_Inner = Nullable_InnerInner?

record Nullable_InnerInner:
  a?: MyType2

type T1st = bool
"
    );
}

#[test]
fn keys_ids_and_order_annotations() {
    let c = convert(&json!({
        "type": "object",
        "x-nova-order": ["z", "a"],
        "properties": {
            "a": {"type": "string", "x-nova-id": 2},
            "z": {"type": "string", "x-nova-id": 1, "x-nova-key": true},
            "bad": {"type": ["string", "null"], "x-nova-key": true, "x-nova-id": 1},
            "s": {"type": "string", "x-nova-len": "..10, utf16"},
            "d": {"type": "string", "x-nova-kind": "decimal", "x-nova-range": "0..1/3"}
        },
        "required": ["z", "a", "bad", "s", "d"]
    }));
    assert_eq!(
        body(&c),
        "\
@root Root

record Root:
  z: string @key @id(1)
  a: string @id(2)
  bad: string?
  s: string @len(..10, utf16)
  d: decimal @range(0..1/3)
"
    );
    assert_eq!(
        c.warnings,
        [
            "#/properties/bad: x-nova-id 1 dropped (not a unique u32)",
            "#/properties/bad: x-nova-key dropped: key fields must be required and non-null"
        ]
    );
}
