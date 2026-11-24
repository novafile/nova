//! Imported definitions (proposal 0008) and classified fields (proposal
//! 0009): the generated `.proto` compiles with `protox`, and values round-trip.

use novafile_schema::{Schema, parse_schema, parse_schema_unlinked};
use novafile_value::Value;
use prost_reflect::prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use protox::file::{ChainFileResolver, File, FileResolver, GoogleFileResolver};

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

/// Compile `proto_file(schema)` with protox and return the descriptor of
/// message `name` in the schema's package.
fn compile(schema: &Schema, name: &str) -> MessageDescriptor {
    let source = novafile_proto::proto_file(schema).unwrap();
    let mut resolver = ChainFileResolver::new();
    resolver.add(Generated(source.clone()));
    resolver.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(resolver);
    compiler.include_imports(true);
    if let Err(e) = compiler.open_file("nova.proto") {
        panic!("generated .proto does not compile: {e:?}\n{source}");
    }

    let pool: DescriptorPool = compiler.descriptor_pool();
    let full = match novafile_proto::package_name(schema) {
        Some(p) => format!("{p}.{name}"),
        None => name.to_owned(),
    };

    pool.get_message_by_name(&full).unwrap_or_else(|| panic!("no message {full} in\n{source}"))
}

/// Whether field `name` of `desc` has `[debug_redact = true]`.
fn redacted(desc: &MessageDescriptor, name: &str) -> bool {
    let field = desc.get_field_by_name(name).unwrap_or_else(|| panic!("no field {name}"));
    // prost-types has no `debug_redact` field yet, so read the option dynamically.
    let options = field.options();
    let redact = options.get_field_by_name("debug_redact").expect("descriptor.proto knows debug_redact");
    redact.as_bool() == Some(true)
}

const COMMON: &str = "@schema acme.test/common@1.0.0

record Address:
  city: string @id(1)
  kind: Kind @id(2)

enum Kind:
  home
  work

enum Contact:
  phone(string) @id(1)
  none @id(2)
";

/// `src` linked against the bundled std and [`COMMON`].
fn linked(src: &str) -> Schema {
    let mut s = parse_schema_unlinked(src).unwrap();
    s.link(&|r| {
        assert_eq!(r.name, "acme.test/common");
        parse_schema(COMMON).map_err(|e| e.message)
    })
    .unwrap();
    s
}

const APP: &str = "@schema acme.test/app@1.0.0
@import acme.test/common@1.0.0 as c
@import novafile.dev/std@0.1.0 as std
@root Person

record Person:
  id: std.Uuid @id(1)
  home: c.Address @id(2)
  contacts: list<c.Contact> @id(3)
  mail?: std.Email @id(4)
";

#[test]
fn imported_definitions_become_mangled_messages() {
    let s = linked(APP);
    let proto = novafile_proto::proto_file(&s).unwrap();
    assert!(proto.contains("message c_Address {"), "{proto}");
    assert!(proto.contains("enum c_Kind {\n  C_KIND_UNSPECIFIED = 0;"), "{proto}");
    assert!(proto.contains("message c_Contact {"), "{proto}");
    assert!(proto.contains("c_Address home = 2;"), "{proto}");
    assert!(!proto.contains("std_"), "std aliases are inlined as string: {proto}");

    let desc = compile(&s, "Person");
    let nova = s
        .resolve(
            &novafile_text::parse("id: 0b7c3c1e-2f4a-4c1e-9a7e-1d2c3b4a5f60\nhome: {city: Graz, kind: !work}\ncontacts: [!phone(\"+43\"), !none]\n").unwrap(),
        )
        .unwrap();
    let bytes = novafile_proto::encode(&nova, &s).unwrap();
    let msg = DynamicMessage::decode(desc.clone(), bytes.as_slice()).unwrap();
    let expected = DynamicMessage::parse_text_format(
        desc,
        r#"id: "0b7c3c1e-2f4a-4c1e-9a7e-1d2c3b4a5f60" home { city: "Graz" kind: C_KIND_WORK } contacts { phone: "+43" } contacts { none {} }"#,
    )
    .unwrap();
    assert_eq!(msg, expected);
    assert_eq!(novafile_proto::decode(&msg.encode_to_vec(), &s).unwrap(), nova);
}

#[test]
fn std_fields_name_their_std_type() {
    let s = linked(&format!("{APP}\ntype Id = std.Uuid\nrecord Ids:\n  ids: list<Id> @id(1)\n"));
    let proto = novafile_proto::proto_file(&s).unwrap();
    assert!(proto.contains("string id = 1; // std.Uuid; novafile.dev/std.Uuid\n"), "{proto}");
    assert!(
        proto.contains("optional string mail = 4; // std.Email; novafile.dev/std.Email; optional field\n"),
        "{proto}"
    );
    assert!(!proto.contains("c_Address home = 2; // c.Address; novafile"), "{proto}");
}

#[test]
fn imported_root_and_name_collisions() {
    let root = linked("@schema acme.test/app@1.0.0\n@import acme.test/common@1.0.0 as c\n@root c.Address\n");
    let desc = compile(&root, "c_Address");
    assert_eq!(desc.fields().count(), 2);

    let clash = linked(
        "@schema acme.test/app@1.0.0\n@import acme.test/common@1.0.0 as C\n@root R\nrecord R:\n  a: C.Address @id(1)\n  b: C_Address @id(2)\nrecord C_Address:\n  x: int @id(1)\n",
    );
    let err = novafile_proto::proto_file(&clash).unwrap_err();
    assert!(err.contains("collides"), "{err}");
}

const CLASSIFIED: &str = "@schema acme.test/vault@1.0.0
@root Service

type Token = string @secret

record Creds @secret:
  user: string @id(1)

enum Owner:
  person(string @personal) @id(1)
  team(string) @id(2)

record Service:
  name: string @id(1)
  db: string @secret @id(2)
  owner?: string @personal @id(3)
  token: Token @id(4) @deprecated
  creds: Creds @id(5)
  keys: list<string @secret> @id(6)
  who: Owner @id(7)
  env: map<string, string> @id(8)
";

#[test]
fn classified_fields_get_debug_redact() {
    let s = parse_schema(CLASSIFIED).unwrap();
    let proto = novafile_proto::proto_file(&s).unwrap();
    assert!(proto.contains("string token = 4 [deprecated = true, debug_redact = true];"), "{proto}");

    let desc = compile(&s, "Service");
    for (field, redact) in [
        ("name", false),
        ("db", true),
        ("owner", true),
        ("token", true),
        ("creds", true),
        ("keys", true),
        ("who", false),
        ("env", false),
    ] {
        assert_eq!(redacted(&desc, field), redact, "{field}\n{proto}");
    }

    let owner = desc.get_field_by_name("who").unwrap().kind();
    let owner = owner.as_message().unwrap();
    assert!(redacted(owner, "person"), "{proto}");
    assert!(!redacted(owner, "team"), "{proto}");
}

#[test]
fn classified_map_values_are_not_representable() {
    let s = parse_schema("@root R\nrecord R:\n  env: map<string, string @secret> @id(1)\n").unwrap();
    let err = novafile_proto::proto_file(&s).unwrap_err();
    assert!(
        err.contains("R.env") && err.contains("map value") && err.contains("no Protobuf mapping"),
        "{err}"
    );

    // Encoding needs no field options, so it still works.
    let v = s.resolve(&novafile_text::parse("env: {a: b}\n").unwrap()).unwrap();
    let bytes = novafile_proto::encode(&v, &s).unwrap();
    assert_eq!(novafile_proto::decode(&bytes, &s).unwrap(), v);

    // A classified map as a whole is a field position.
    let s = parse_schema("@root R\ntype Env = map<string, string> @secret\nrecord R:\n  env: Env @id(1)\n").unwrap();
    assert!(redacted(&compile(&s, "R"), "env"));
}

#[test]
fn classified_values_round_trip() {
    let s = parse_schema(CLASSIFIED).unwrap();
    let v = s
        .resolve(&novafile_text::parse("name: n\ndb: d\ntoken: t\ncreds: {user: u}\nkeys: [k1, k2]\nwho: !person(p)\nenv: {}\n").unwrap())
        .unwrap();
    let bytes = novafile_proto::encode(&v, &s).unwrap();
    let decoded: Value = novafile_proto::decode(&bytes, &s).unwrap();
    assert_eq!(decoded, v);
}
