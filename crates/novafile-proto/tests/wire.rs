//! Hand-computed wire bytes, decoder robustness and error reporting.

use novafile_proto::{decode, encode, proto_file};
use novafile_schema::{Schema, parse_schema};
use novafile_value::{Record, Value};

fn schema(src: &str) -> Schema {
    parse_schema(src).unwrap_or_else(|e| panic!("{e:?}"))
}

fn value(schema: &Schema, text: &str) -> Value {
    schema.resolve(&novafile_text::parse(text).unwrap()).unwrap()
}

fn field<'v>(v: &'v Value, name: &str) -> Option<&'v Value> {
    v.as_record().unwrap().get(name)
}

const POINT: &str = "@root Point

record Point:
  label: string @id(3)
  x: i32 @id(1)
  y: i32 @id(2)
  scores: list<u32> @id(4)
  kind?: Kind @id(5)

enum Kind:
  plain
  fancy
";

#[test]
fn hand_computed_bytes() {
    let s = schema(POINT);
    let v = value(&s, "label: a\nx: 1\ny: -1\nscores: [1, 2, 300]\nkind: !fancy\n");
    let bytes = encode(&v, &s).unwrap();
    #[rustfmt::skip]
    let expected = [
        0x08, 0x02,             // 1: sint32 1 (zigzag)
        0x10, 0x01,             // 2: sint32 -1 (zigzag)
        0x1a, 0x01, b'a',       // 3: "a"
        0x22, 0x04, 0x01, 0x02, 0xac, 0x02, // 4: packed [1, 2, 300]
        0x28, 0x02,             // 5: KIND_FANCY
    ];
    assert_eq!(bytes, expected);
    assert_eq!(decode(&bytes, &s).unwrap(), v);
}

#[test]
fn anonymous_schema_has_no_package() {
    let proto = proto_file(&schema(POINT)).unwrap();
    assert!(!proto.contains("package"), "{proto}");
    assert!(
        proto.contains("enum Kind {\n  KIND_UNSPECIFIED = 0;\n  KIND_PLAIN = 1; // plain\n  KIND_FANCY = 2; // fancy\n}"),
        "{proto}"
    );
    assert!(proto.contains("optional Kind kind = 5;"), "{proto}");
}

#[test]
fn decoder_accepts_any_order_unknown_fields_and_unpacked() {
    let s = schema(POINT);
    #[rustfmt::skip]
    let bytes = [
        0x1a, 0x01, b'z',       // 3: "z" (overwritten below)
        0x48, 0x96, 0x01,       // 9: unknown varint
        0x51, 1, 2, 3, 4, 5, 6, 7, 8, // 10: unknown i64
        0x5a, 0x02, 0xff, 0xff, // 11: unknown len (not UTF-8; never looked at)
        0x65, 1, 2, 3, 4,       // 12: unknown i32
        0x10, 0x03,             // 2: -2
        0x20, 0x07,             // 4: unpacked 7
        0x22, 0x01, 0x08,       // 4: packed [8]
        0x20, 0x09,             // 4: unpacked 9
        0x08, 0x04,             // 1: 2 (overwritten below)
        0x1a, 0x01, b'b',       // 3: "b" (last one wins)
        0x08, 0x06,             // 1: 3
    ];
    let v = decode(&bytes, &s).unwrap();
    assert_eq!(v, value(&s, "label: b\nx: 3\ny: -2\nscores: [7, 8, 9]\n"));
    assert_eq!(field(&v, "kind"), None, "a missing optional field stays absent");
}

#[test]
fn malformed_input_is_an_error() {
    let s = schema(POINT);
    for (bytes, why) in [
        (&[0x0b][..], "group start"),
        (&[0x0c], "group end"),
        (&[0x0e, 0x00], "wire type 6"),
        (&[0x0f, 0x00], "wire type 7"),
        (&[0x00, 0x00], "field number 0"),
        (&[0x08], "truncated varint"),
        (&[0x08, 0x80], "truncated varint"),
        (&[0x1a, 0x05, b'a'], "length beyond the input"),
        (&[0x1a, 0xff, 0xff, 0xff, 0xff, 0x0f], "huge length"),
        (&[0x1a, 0x02, 0xc3, 0x28], "invalid UTF-8"),
        (&[0x09, 0, 0, 0, 0, 0, 0, 0, 0], "wrong wire type for a known field"),
        (&[0x28, 0x00], "enum value 0 (UNSPECIFIED)"),
        (&[0x28, 0x03], "unknown enum value"),
        (&[0x22, 0x01, 0x80], "truncated packed varint"),
        (&[0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01], "11-byte varint"),
    ] {
        assert!(decode(bytes, &s).is_err(), "{why}: {bytes:?} decoded");
    }
}

const RICH: &str = include_str!("data/rich.novs");

/// A deterministic pseudo-random generator for the fuzz-style tests.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
#[expect(clippy::cast_possible_truncation, reason = "random values are deliberately cut to indices and bytes")]
fn truncations_and_mutations_never_panic() {
    let s = schema(RICH);
    let v = value(
        &s,
        r#"
name: n
count: 1
small: 1
big: 1
ratio: 1.0
price: @decimal("1.5")
share: @rational("1/3")
blob: @bytes("AA==")
when: @timestamp("2026-01-01T00:00:00Z")
took: @duration("1s")
color: !red
shape: !rect({w: 1, h: 2})
shapes: [!point, !label(x)]
nums: [1, 2]
floats: [1.0]
colors: [!green]
tags: [t]
attrs: {a: b}
rects: @map{1 => {w: 1, h: 1}}
note: x
child: {w: 1, h: 1}
huge: 5
history: [{w: 1, h: 1}]
old: 1
weights: @map{1 => 1.0}
wait: @duration("1s")
"#,
    );
    let bytes = encode(&v, &s).unwrap();
    assert_eq!(decode(&bytes, &s).unwrap(), v);
    for n in 0..bytes.len() {
        let _ = decode(&bytes[..n], &s);
    }
    let mut rng = Lcg(42);
    for _ in 0..5000 {
        let mut b = bytes.clone();
        for _ in 0..=(rng.next() % 4) {
            let i = (rng.next() as usize) % b.len();
            b[i] = rng.next() as u8;
        }
        let _ = decode(&b, &s);
    }
    for _ in 0..2000 {
        let len = (rng.next() % 64) as usize;
        let b: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = decode(&b, &s);
    }
}

const LINKED: &str = "@root Node

record Node:
  value: u32 @id(1)
  next: Node? @id(2)
";

#[test]
#[expect(clippy::cast_possible_truncation, reason = "varint bytes keep the low 7 bits")]
fn recursion_depth_is_bounded() {
    let s = schema(LINKED);
    // 100 nodes are fine both ways.
    let mut text = String::from("null");
    for i in 0..100 {
        text = format!("{{value: {i}, next: {text}}}");
    }
    let v = value(&s, &text);
    assert_eq!(decode(&encode(&v, &s).unwrap(), &s).unwrap(), v);

    // 200 nested messages on the wire: refused, not a stack overflow.
    let mut bytes: Vec<u8> = Vec::new();
    for _ in 0..200 {
        let mut outer = vec![0x12];
        let mut n = bytes.len();
        while n >= 0x80 {
            outer.push((n as u8) | 0x80);
            n >>= 7;
        }
        outer.push(n as u8);
        outer.extend(bytes);
        bytes = outer;
    }
    let err = decode(&bytes, &s).unwrap_err();
    assert!(err.contains("deeper than 128"), "{err}");

    let mut deep = Value::Null;
    for _ in 0..200 {
        let mut r = novafile_value::Record::new();
        r.insert("value", Value::Int(1)).unwrap();
        r.insert("next", deep).unwrap();
        deep = Value::Record(r);
    }
    let err = encode(&deep, &s).unwrap_err();
    assert!(err.contains("deeper than 128"), "{err}");
}

#[test]
fn missing_required_message_reads_as_empty_and_is_validated() {
    // A record field absent from the wire is decoded from zero bytes, so its
    // own fields get zero values, which validation then checks.
    let s = schema("@root Outer\n\nrecord Outer:\n  inner: Inner @id(1)\n\nrecord Inner:\n  n: u8 @id(1) @range(1..)\n");
    let err = decode(&[], &s).unwrap_err();
    assert!(err.contains("inner.n"), "{err}");
    let ok = decode(&[0x0a, 0x02, 0x08, 0x05], &s).unwrap();
    assert_eq!(ok, value(&s, "inner: {n: 5}"));
}

#[test]
fn oneof_last_member_wins_and_merges() {
    let s = schema("@root U\n\nenum U:\n  a(R)\n  b(string)\n\nrecord R:\n  x: u32 @id(1)\n  y: u32 @id(2)\n");
    let proto = proto_file(&s).unwrap();
    assert!(proto.contains("message U {\n  oneof variant {\n    R a = 1;"), "{proto}");
    // b, then a{x:1}, then a{y:2}: the a occurrences merge.
    let bytes = [0x12, 0x01, b'q', 0x0a, 0x02, 0x08, 0x01, 0x0a, 0x02, 0x10, 0x02];
    assert_eq!(decode(&bytes, &s).unwrap(), Value::Variant("a".into(), Some(Box::new(rect(1, 2)))));
    // a, then b: b wins.
    let bytes = [0x0a, 0x02, 0x08, 0x01, 0x12, 0x01, b'q'];
    assert_eq!(
        decode(&bytes, &s).unwrap(),
        Value::Variant("b".into(), Some(Box::new(Value::String("q".into()))))
    );
    assert!(decode(&[], &s).unwrap_err().contains("no variant of U is set"));
    // Encoding the root union.
    let v = Value::Variant("b".into(), Some(Box::new(Value::String("q".into()))));
    assert_eq!(encode(&v, &s).unwrap(), [0x12, 0x01, b'q']);
}

#[test]
fn oneof_name_avoids_variant_names() {
    let s = schema("@root U\n\nenum U:\n  variant(string)\n  other\n");
    let proto = proto_file(&s).unwrap();
    assert!(proto.contains("oneof variant_ {"), "{proto}");
    assert!(proto.contains("google.protobuf.Empty other = 2;"), "{proto}");
}

#[test]
fn well_known_types_on_the_wire() {
    let s = schema("@root T\n\nrecord T:\n  at: timestamp @id(1)\n  d: duration @id(2)\n");
    let v = value(&s, "at: @timestamp(\"1969-12-31T23:59:59.5Z\")\nd: @duration(\"-1.5s\")\n");
    let bytes = encode(&v, &s).unwrap();
    #[rustfmt::skip]
    let expected = [
        0x0a, 0x11, // at: Timestamp
            0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, // seconds -1
            0x10, 0x80, 0xca, 0xb5, 0xee, 0x01, // nanos 500_000_000
        0x12, 0x16, // d: Duration
            0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, // seconds -1
            0x10, 0x80, 0xb6, 0xca, 0x91, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x01, // nanos -500_000_000
    ];
    assert_eq!(bytes, expected);
    assert_eq!(decode(&bytes, &s).unwrap(), v);

    #[rustfmt::skip]
    let bad: [&[u8]; 3] = [
        // d: seconds 1, nanos -1 (different signs)
        &[0x12, 0x0d, 0x08, 0x01, 0x10, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
        // d: nanos 1_000_000_000 (out of range)
        &[0x12, 0x06, 0x10, 0x80, 0x94, 0xeb, 0xdc, 0x03],
        // at: nanos -1
        &[0x0a, 0x0b, 0x10, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
    ];
    for b in bad {
        assert!(decode(b, &s).is_err(), "{b:?}");
    }
}

#[test]
fn decimal_and_rational_text() {
    let s = schema("@root T\n\nrecord T:\n  d: decimal @id(1)\n  r: rational @id(2)\n");
    let v = value(&s, "d: @decimal(\"-0.000123\")\nr: @rational(\"-1/3\")\n");
    let bytes = encode(&v, &s).unwrap();
    let mut expected = vec![0x0a, 9];
    expected.extend(b"-0.000123");
    expected.extend([0x12, 4]);
    expected.extend(b"-1/3");
    assert_eq!(bytes, expected);
    assert_eq!(decode(&bytes, &s).unwrap(), v);
    // Empty strings are the proto3 zero value.
    assert_eq!(
        decode(&[0x0a, 0x00, 0x12, 0x00], &s).unwrap(),
        value(&s, "d: @decimal(\"0\")\nr: @rational(\"0/1\")\n")
    );
    assert!(decode(&[0x0a, 0x01, b'x'], &s).is_err());
    assert!(decode(&[0x12, 0x03, b'1', b'/', b'0'], &s).is_err());
}

#[test]
fn integer_ranges() {
    let s = schema("@root T\n\nrecord T:\n  big: int @id(1)\n  small: i8 @id(2)\n");
    let err = encode(&value(&s, "big: 9223372036854775808\nsmall: 0\n"), &s).unwrap_err();
    assert!(err.contains("big") && err.contains("sint64"), "{err}");
    // i8 travels as sint32; out-of-range values from the wire fail validation.
    let err = decode(&[0x10, 0x80, 0x02], &s).unwrap_err();
    assert!(err.contains("small"), "{err}");
    let v = value(&s, "big: -9223372036854775808\nsmall: -128\n");
    assert_eq!(decode(&encode(&v, &s).unwrap(), &s).unwrap(), v);
}

#[test]
fn nulls_and_optional_collections() {
    let s = schema("@root T\n\nrecord T:\n  a: string? @id(1)\n  b?: list<u8> @id(2)\n  c?: string @id(3)\n  d: string @id(4)\n");
    let v = value(&s, "a: null\nb: [1]\nd: \"\"\n");
    let bytes = encode(&v, &s).unwrap();
    assert_eq!(bytes, [0x12, 0x01, 0x01, 0x22, 0x00]);
    assert_eq!(decode(&bytes, &s).unwrap(), v);
    // A missing nullable field reads as null.
    assert_eq!(decode(&[], &s).unwrap(), value(&s, "a: null\nd: \"\"\n"));
    // Documented proto3 limitation: an empty optional collection decodes as absent.
    let v = value(&s, "a: x\nb: []\nd: y\n");
    assert_eq!(decode(&encode(&v, &s).unwrap(), &s).unwrap(), value(&s, "a: x\nd: y\n"));
    // null on a non-nullable field is rejected.
    let err = encode(&novafile_text::parse("a: x\nd: null\n").unwrap(), &s).unwrap_err();
    assert!(err.contains('d'), "{err}");
}

fn rect(x: i128, y: i128) -> Value {
    let mut r = novafile_value::Record::new();
    r.insert("x", Value::Int(x)).unwrap();
    r.insert("y", Value::Int(y)).unwrap();
    Value::Record(r)
}

#[test]
fn maps_last_entry_wins() {
    let s = schema("@root T\n\nrecord T:\n  m: map<string, u32> @id(1)\n");
    // {a: 1}, {a: 2}, {b: (missing value)}
    let bytes = [
        0x0a, 0x05, 0x0a, 0x01, b'a', 0x10, 0x01, 0x0a, 0x05, 0x0a, 0x01, b'a', 0x10, 0x02, 0x0a, 0x03, 0x0a, 0x01, b'b',
    ];
    assert_eq!(decode(&bytes, &s).unwrap(), value(&s, "m: {a: 2, b: 0}"));
}

#[test]
fn schema_errors_name_the_field() {
    let cases = [
        ("record R:\n  a: u8\n", "R.a: field has no @id(n)"),
        ("record R:\n  a: u8 @id(19000)\n", "R.a: @id(19000) is in the range 19000..=19999"),
        (
            "record R:\n  a: u8 @id(536870912)\n",
            "R.a: @id(536870912) is not a valid Protobuf field number",
        ),
        ("record R:\n  a: u8 @id(0)\n", "R.a: @id(0) is not a valid"),
        ("record R:\n  a: (u8, u8) @id(1)\n", "R.a: a tuple has no Protobuf mapping"),
        ("record R:\n  a: any @id(1)\n", "R.a: `any` has no Protobuf mapping"),
        ("record R:\n  a: list<list<u8>> @id(1)\n", "R.a: a list or set of lists"),
        ("record R:\n  a: list<u8?> @id(1)\n", "R.a: a list or set with nullable elements"),
        ("record R:\n  a: list<u8>? @id(1)\n", "R.a: a nullable list"),
        ("record R:\n  a: map<float, u8> @id(1)\n", "R.a: a map with float keys"),
        ("record R:\n  a: map<string, list<u8>> @id(1)\n", "R.a: a map whose values are lists"),
        (
            "record R:\n  a: E @id(1)\n\nenum E:\n  x(list<u8>)\n",
            "E.x: a list, set or map variant payload",
        ),
        ("record R:\n  a: E @id(1)\n\nenum E:\n  x(u8?)\n", "E.x: a nullable variant payload"),
        ("record R:\n  a: E @id(1)\n\nenum E:\n  unspecified\n", "collides"),
    ];
    for (body, expected) in cases {
        let s = schema(&format!("@root R\n\n{body}"));
        let err = proto_file(&s).unwrap_err();
        assert!(err.contains(expected), "{body}\n=> {err}");
        assert!(encode(&Value::Record(Record::default()), &s).is_err());
        assert!(decode(&[], &s).is_err());
    }
    // Unreachable definitions are not checked.
    assert!(proto_file(&schema("@root R\n\nrecord R:\n  a: u8 @id(1)\n\nrecord Other:\n  t: (u8,)\n")).is_ok());
    let err = proto_file(&schema("@root E\n\nenum E:\n  a\n  b\n")).unwrap_err();
    assert!(err.contains("must be a record"), "{err}");
    let err = proto_file(&schema("record R:\n  a: u8 @id(1)\n")).unwrap_err();
    assert!(err.contains("@root"), "{err}");
}

#[test]
fn open_record_extras_cannot_be_encoded() {
    let s = schema("@root R\n\nrecord R @open:\n  a: u8 @id(1)\n");
    let err = encode(&novafile_text::parse("a: 1\nextra: 2\n").unwrap(), &s).unwrap_err();
    assert!(err.contains("extra"), "{err}");
}

#[test]
fn infrastructure_proto_golden() {
    let mut src = include_str!("../../../examples/infrastructure.novs").to_owned();
    for (line, id) in [
        ("environment: string", 1),
        ("region: string", 2),
        ("deployed_at: timestamp", 3),
        ("services: Service[]", 5),
    ] {
        src = src.replace(&format!("  {line}\n"), &format!("  {line} @id({id})\n"));
    }
    src = src.replace("  status: Status = !active\n", "  status: Status @id(4) = !active\n");
    let proto = proto_file(&schema(&src)).unwrap();
    let expected = r#"// Generated by novafile-proto from schema infrastructure@1.0.0.
// Root message: Infrastructure. Trailing comments give each field's NOVA type.
syntax = "proto3";

package infrastructure.v1;

import "google/protobuf/empty.proto";
import "google/protobuf/timestamp.proto";

message Service {
  string id = 1; // string
  string image = 2; // string @len(1..)
  uint32 port = 3; // Port
  uint32 replicas = 4; // u16 @range(0..100)
  optional bool enabled = 5; // bool; default true
  repeated string tags = 6; // set<string>; optional field
}

message Status {
  oneof variant {
    google.protobuf.Empty active = 1; // active (unit variant)
    google.protobuf.Empty draining = 2; // draining (unit variant)
    string failed = 3; // failed(string)
  }
}

message Infrastructure {
  string environment = 1; // string
  string region = 2; // string
  google.protobuf.Timestamp deployed_at = 3; // timestamp
  Status status = 4; // Status; default !active
  repeated Service services = 5; // list<Service>
}
"#;
    assert_eq!(proto, expected);
}
