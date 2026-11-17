//! Interoperability with an independent Protobuf implementation: the
//! generated `.proto` is compiled by `protox` (a pure-Rust protoc), and
//! messages are cross-checked with `prost-reflect` dynamic messages in both
//! directions.

use nova_schema::{parse_schema, Schema};
use nova_value::Value;
use prost_reflect::prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MapKey, MessageDescriptor, Value as PValue};
use protox::file::{ChainFileResolver, File, FileResolver, GoogleFileResolver};

const RICH: &str = include_str!("data/rich.novs");
const INFRA_SCHEMA: &str = include_str!("../../../examples/infrastructure.novs");
const INFRA_DATA: &str = include_str!("../../../examples/infrastructure.nova");

/// Serves the generated file from memory.
struct Generated(String);

impl FileResolver for Generated {
    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        if name == "nova.proto" {
            File::from_source(name, &self.0)
        } else {
            Err(protox::Error::file_not_found(name))
        }
    }
}

/// Compile `proto_file(schema)` with protox and return the root message descriptor.
fn compile(schema: &Schema) -> MessageDescriptor {
    let source = nova_proto::proto_file(schema).unwrap();
    let mut resolver = ChainFileResolver::new();
    resolver.add(Generated(source.clone()));
    resolver.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(resolver);
    compiler.include_imports(true);
    if let Err(e) = compiler.open_file("nova.proto") {
        panic!("generated .proto does not compile: {e:?}\n{source}");
    }
    let pool: DescriptorPool = compiler.descriptor_pool();
    let root = schema.root.as_deref().unwrap().replace('-', "_");
    let full = match nova_proto::package_name(schema) {
        Some(p) => format!("{p}.{root}"),
        None => root,
    };
    pool.get_message_by_name(&full).unwrap_or_else(|| panic!("no message {full} in\n{source}"))
}

fn value(schema: &Schema, text: &str) -> Value {
    schema.resolve(&nova_text::parse(text).unwrap()).unwrap()
}

/// NOVA → bytes → DynamicMessage, compared with a text-format expectation.
fn check_nova_to_proto(desc: &MessageDescriptor, schema: &Schema, nova: &Value, text_format: &str) -> DynamicMessage {
    let bytes = nova_proto::encode(nova, schema).unwrap();
    let dynamic = DynamicMessage::decode(desc.clone(), bytes.as_slice()).unwrap();
    let expected = DynamicMessage::parse_text_format(desc.clone(), text_format).unwrap();
    assert_eq!(dynamic, expected, "\nencoded: {dynamic}\nexpected: {expected}");
    dynamic
}

/// DynamicMessage (text format) → prost bytes → NOVA, compared with the expectation.
fn check_proto_to_nova(desc: &MessageDescriptor, schema: &Schema, text_format: &str, expected: &Value) {
    let msg = DynamicMessage::parse_text_format(desc.clone(), text_format).unwrap();
    let bytes = msg.encode_to_vec();
    let decoded = nova_proto::decode(&bytes, schema).unwrap();
    assert_eq!(
        &decoded,
        expected,
        "\ndecoded:  {}\nexpected: {}",
        nova_text::flow_string(&decoded),
        nova_text::flow_string(expected)
    );
}

const RICH_VALUE: &str = r#"
name: widget
count: -9000000000
small: -5
big: 18446744073709551615
ratio: 0.25
price: @decimal("12.50")
share: @rational("22/7")
blob: @bytes("AP8H")
flag: false
when: @timestamp("2026-11-17T16:00:00.5Z")
took: @duration("-1h30m0.25s")
color: !dark-blue
shape: !rect({w: 3, h: 4})
shapes: [!point, !circle(1.5), !label("hi"), !tint(!green), !at(@timestamp("1970-01-01T00:00:01Z"))]
nums: [1, -2, 300]
floats: [1.5, -2.25]
colors: [!red, !green, !dark-blue]
tags: [a, b]
attrs: {x: "1", y: "2"}
rects: @map{7 => {w: 1, h: 2}, 4294967295 => {w: 5, h: 6}}
nick: bob
note: null
child: {w: 9, h: 10}
level: 42
maybe_color: !green
huge: -9223372036854775808
history: [{w: 1, h: 1}, {w: 2, h: 2}]
old: 7
weights: @map{-1 => 0.5, 9007199254740993 => 2.0}
wait: @duration("2d")
"#;

const RICH_TEXT_FORMAT: &str = r#"
name: "widget"
count: -9000000000
small: -5
big: 18446744073709551615
ratio: 0.25
price: "12.5"
share: "22/7"
blob: "\x00\xff\x07"
flag: false
when { seconds: 1794931200 nanos: 500000000 }
took { seconds: -5400 nanos: -250000000 }
color: COLOR_DARK_BLUE
shape { rect { w: 3 h: 4 } }
shapes { point {} }
shapes { circle: 1.5 }
shapes { label: "hi" }
shapes { tint: COLOR_GREEN }
shapes { at { seconds: 1 nanos: 0 } }
nums: [1, -2, 300]
floats: [1.5, -2.25]
colors: [COLOR_RED, COLOR_GREEN, COLOR_DARK_BLUE]
tags: ["a", "b"]
attrs { key: "x" value: "1" }
attrs { key: "y" value: "2" }
rects { key: 7 value { w: 1 h: 2 } }
rects { key: 4294967295 value { w: 5 h: 6 } }
nick: "bob"
child { w: 9 h: 10 }
level: 42
maybe_color: COLOR_GREEN
huge: -9223372036854775808
history { w: 1 h: 1 }
history { w: 2 h: 2 }
old: 7
weights { key: -1 value: 0.5 }
weights { key: 9007199254740993 value: 2.0 }
wait { seconds: 172800 nanos: 0 }
"#;

#[test]
fn rich_schema_compiles_with_protox() {
    let schema = parse_schema(RICH).unwrap();
    let desc = compile(&schema);
    assert_eq!(desc.full_name(), "acme_rich_test.v3.Doc");
    let field = |n: &str| desc.get_field_by_name(n).unwrap_or_else(|| panic!("no field {n}"));
    assert_eq!(field("wait").number(), 536_870_911);
    assert!(field("nums").is_packed());
    assert!(field("floats").is_packed());
    assert!(field("colors").is_packed());
    assert!(field("rects").is_map());
    assert!(field("nick").supports_presence());
    assert!(field("note").supports_presence());
    assert!(field("flag").supports_presence(), "fields with defaults get explicit presence");
    assert!(!field("name").supports_presence());
    let shape = field("shape").kind();
    let shape = shape.as_message().unwrap();
    let oneof = shape.oneofs().next().unwrap();
    assert_eq!(oneof.name(), "variant");
    let members: Vec<_> = oneof.fields().map(|f| (f.name().to_owned(), f.number())).collect();
    assert_eq!(
        members,
        [("point", 1), ("circle", 2), ("label", 3), ("rect", 4), ("tint", 5), ("at", 6)].map(|(n, i)| (n.to_owned(), i))
    );
    let color = field("color").kind();
    let color = color.as_enum().unwrap();
    let values: Vec<_> = color.values().map(|v| (v.name().to_owned(), v.number())).collect();
    assert_eq!(
        values,
        [("COLOR_UNSPECIFIED", 0), ("COLOR_RED", 1), ("COLOR_GREEN", 2), ("COLOR_DARK_BLUE", 3)].map(|(n, i)| (n.to_owned(), i))
    );
}

#[test]
fn rich_value_nova_to_prost() {
    let schema = parse_schema(RICH).unwrap();
    let desc = compile(&schema);
    let nova = value(&schema, RICH_VALUE);
    let msg = check_nova_to_proto(&desc, &schema, &nova, RICH_TEXT_FORMAT);
    // A few explicit spot checks, independent of the text-format parser.
    let get = |n: &str| msg.get_field_by_name(n).unwrap().into_owned();
    assert_eq!(get("count"), PValue::I64(-9_000_000_000));
    assert_eq!(get("big"), PValue::U64(u64::MAX));
    assert_eq!(get("price"), PValue::String("12.5".into()));
    assert_eq!(get("color"), PValue::EnumNumber(3));
    assert!(!msg.has_field_by_name("note"), "null is omitted");
    let PValue::Map(rects) = get("rects") else { panic!() };
    assert!(rects.contains_key(&MapKey::U32(u32::MAX)));
    let PValue::Message(took) = get("took") else { panic!() };
    assert_eq!(took.get_field_by_name("seconds").unwrap().as_i64(), Some(-5400));
    assert_eq!(took.get_field_by_name("nanos").unwrap().as_i32(), Some(-250_000_000));
}

#[test]
fn rich_value_prost_to_nova() {
    let schema = parse_schema(RICH).unwrap();
    let desc = compile(&schema);
    let nova = value(&schema, RICH_VALUE);
    check_proto_to_nova(&desc, &schema, RICH_TEXT_FORMAT, &nova);
    // And our own round trip.
    let bytes = nova_proto::encode(&nova, &schema).unwrap();
    assert_eq!(nova_proto::decode(&bytes, &schema).unwrap(), nova);
}

#[test]
fn prost_omits_zero_values_and_defaults_apply() {
    let schema = parse_schema(RICH).unwrap();
    let desc = compile(&schema);
    // prost-reflect omits proto3 zero values; absent fields take NOVA
    // defaults, absent optional fields stay absent, and absent nullable
    // fields read as null.
    let text = r#"
        name: "z"
        color: COLOR_RED
        shape { point {} }
    "#;
    let expected = value(
        &schema,
        r#"
name: z
count: 0
small: 0
big: 0
ratio: 0.0
price: @decimal("0")
share: @rational("0/1")
blob: @bytes("")
flag: true
when: @timestamp("1970-01-01T00:00:00Z")
took: @duration("0s")
color: !red
shape: !point
shapes: []
nums: []
floats: []
colors: []
tags: @set[]
attrs: @map{}
rects: @map{}
note: null
child: null
level: 50
huge: 0
history: []
old: 0
weights: @map{}
wait: @duration("0s")
        "#,
    );
    check_proto_to_nova(&desc, &schema, text, &expected);
    // An explicit `false` for a field defaulting to `true` survives, because
    // the field has explicit presence.
    let text = r#"name: "z" color: COLOR_RED shape { point {} } flag: false"#;
    let decoded = nova_proto::decode(&DynamicMessage::parse_text_format(desc.clone(), text).unwrap().encode_to_vec(), &schema).unwrap();
    assert_eq!(decoded.as_record().unwrap().get("flag"), Some(&Value::Bool(false)));
}

#[test]
fn prost_unpacked_and_merged_input() {
    let schema = parse_schema(RICH).unwrap();
    let desc = compile(&schema);
    // Encode `nums` unpacked (as proto2 writers do) by hand, append it to a
    // prost-produced message, and repeat a singular message to check merging.
    let base = DynamicMessage::parse_text_format(desc.clone(), r#"name: "m" color: COLOR_RED shape { point {} } child { w: 1 }"#).unwrap();
    let mut bytes = base.encode_to_vec();
    bytes.extend([0x78, 0x02, 0x78, 0x03]); // field 15 (sint32), varint wire type: 1, -2
    bytes.extend([0xba, 0x01, 0x02, 0x10, 0x05]); // field 23 again: child { h: 5 }, merged
    let v = nova_proto::decode(&bytes, &schema).unwrap();
    let r = v.as_record().unwrap();
    assert_eq!(r.get("nums"), Some(&Value::List(vec![Value::Int(1), Value::Int(-2)])));
    let child = r.get("child").unwrap().as_record().unwrap();
    assert_eq!((child.get("w"), child.get("h")), (Some(&Value::Int(1)), Some(&Value::Int(5))));
}

#[test]
fn fields_without_ids_are_rejected() {
    let schema = parse_schema(&INFRA_SCHEMA.replace("  region: string @id(2)\n", "  region: string\n")).unwrap();
    let err = nova_proto::proto_file(&schema).unwrap_err();
    assert!(err.contains("Infrastructure.region") && err.contains("@id"), "{err}");
}

/// The example schema (every field carries `@id`).
fn infra_with_ids() -> Schema {
    parse_schema(INFRA_SCHEMA).unwrap()
}

#[test]
fn infrastructure_example_round_trips_through_prost() {
    let schema = infra_with_ids();
    let proto = nova_proto::proto_file(&schema).unwrap();
    assert!(proto.contains("package infrastructure.v1;"), "{proto}");
    assert!(proto.contains("import \"google/protobuf/timestamp.proto\";"), "{proto}");
    assert!(proto.contains("import \"google/protobuf/empty.proto\";"), "{proto}");
    let desc = compile(&schema);
    let nova = schema.resolve(&nova_text::parse(INFRA_DATA).unwrap()).unwrap();
    let text = r#"
        environment: "production"
        region: "eu-central-1"
        deployed_at { seconds: 1794931200 nanos: 0 }
        status { active {} }
        services { id: "auth" image: "ghcr.io/example/auth:v2" port: 8080 replicas: 3 enabled: true }
        services { id: "api" image: "ghcr.io/example/api:v5" port: 3000 replicas: 5 enabled: true tags: ["public", "edge"] }
    "#;
    check_nova_to_proto(&desc, &schema, &nova, text);
    check_proto_to_nova(&desc, &schema, text, &nova);

    // A payload variant and prost's own output for it.
    let failed = r#"environment: "staging" region: "r" deployed_at { seconds: 1 } status { failed: "disk full" }"#;
    let msg = DynamicMessage::parse_text_format(desc.clone(), failed).unwrap();
    let v = nova_proto::decode(&msg.encode_to_vec(), &schema).unwrap();
    assert_eq!(
        v.as_record().unwrap().get("status"),
        Some(&Value::Variant("failed".into(), Some(Box::new(Value::String("disk full".into())))))
    );
    assert_eq!(v.as_record().unwrap().get("services"), Some(&Value::List(vec![])));
}

#[test]
fn constraint_violations_from_prost_are_rejected() {
    let schema = infra_with_ids();
    let desc = compile(&schema);
    // port 0 violates `Port = u16 @range(1..65535)`; replicas 70000 overflows u16.
    for bad in [
        r#"environment: "e" region: "r" deployed_at {} status { active {} } services { id: "a" image: "i" port: 0 }"#,
        r#"environment: "e" region: "r" deployed_at {} status { active {} } services { id: "a" image: "i" port: 1 replicas: 70000 }"#,
        r#"environment: "e" region: "r" deployed_at {} status { active {} } services { id: "a" port: 1 }"#,
    ] {
        let bytes = DynamicMessage::parse_text_format(desc.clone(), bad).unwrap().encode_to_vec();
        let err = nova_proto::decode(&bytes, &schema).unwrap_err();
        assert!(err.contains("services[0]"), "{err}");
    }
}
