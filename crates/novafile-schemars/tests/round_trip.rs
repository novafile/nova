//! NOVA schema → JSON Schema → NOVA schema.

use novafile_schema::{Schema, check_compatibility, parse_schema};
use novafile_schemars::{from_json_schema, to_json_schema};

mod common;
use common::{KITCHEN_SINK, example};

/// `old` → JSON Schema → back: no breaking change either way.
fn assert_round_trip(old: &Schema) -> Schema {
    let json = to_json_schema(old);
    let c = from_json_schema(&json, None, None).unwrap();
    assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
    let new = c.schema;
    for (a, b) in [(old, &new), (&new, old)] {
        let breaking: Vec<_> = check_compatibility(a, b).into_iter().filter(|c| c.breaking).collect();
        assert!(breaking.is_empty(), "{breaking:#?}\n{}", serde_json::to_string_pretty(&json).unwrap());
    }

    // Re-parsable, and converting again is a fixed point.
    let text = new.to_string();
    assert_eq!(parse_schema(&text).unwrap().to_string(), text);
    assert_eq!(to_json_schema(&new), json);
    new
}

#[test]
fn infrastructure_round_trips_exactly() {
    let old = parse_schema(&example("infrastructure.novs")).unwrap();
    let new = assert_round_trip(&old);
    // Nothing is approximated: the canonical text and fingerprint are unchanged.
    assert_eq!(new.to_string(), old.to_string());
    assert_eq!(new.fingerprint(), old.fingerprint());
    let v = novafile_schema::check_versions(&old, &new).unwrap();
    assert_eq!(v.kind, novafile_schema::ChangeKind::None);
}

#[test]
fn every_kind_round_trips_exactly() {
    let old = parse_schema(KITCHEN_SINK).unwrap();
    let new = assert_round_trip(&old);
    assert_eq!(new.to_string(), old.to_string());
}

#[test]
fn infrastructure_json_schema_shape() {
    let json = to_json_schema(&parse_schema(&example("infrastructure.novs")).unwrap());
    assert_eq!(json["$schema"], "https://json-schema.org/draft/2020-12/schema");
    // A local name has no global identity, so no `$id`; the reference rides along.
    assert_eq!(json.get("$id"), None);
    assert_eq!(json["x-nova-schema"], "infrastructure@1.0.0");
    assert_eq!(json["$ref"], "#/$defs/Infrastructure");
    assert_eq!(
        json["$defs"]["Port"],
        serde_json::json!({"type": "integer", "format": "uint16", "minimum": 1, "maximum": 65535})
    );
    assert_eq!(
        json["$defs"]["Status"],
        serde_json::json!({"oneOf": [
            {"type": "string", "enum": ["active", "draining"]},
            {"type": "object", "properties": {"failed": {"type": "string"}}, "required": ["failed"], "additionalProperties": false}
        ]})
    );
    let service = &json["$defs"]["Service"];
    assert_eq!(service["additionalProperties"], false);
    assert_eq!(service["required"], serde_json::json!(["id", "image", "port", "replicas"]));
    assert_eq!(service["properties"]["enabled"]["default"], true);
    assert_eq!(service["properties"]["id"]["x-nova-key"], true);
    assert_eq!(service["properties"]["image"]["minLength"], 1);
    assert_eq!(
        service["properties"]["tags"],
        serde_json::json!({"type": "array", "items": {"type": "string"}, "uniqueItems": true, "x-nova-id": 6})
    );
    assert_eq!(json["$defs"]["Infrastructure"]["properties"]["status"]["default"], "active");
}

#[test]
fn patterns_and_literals_round_trip_exactly() {
    let old = parse_schema(
        "@root R\ntype Op = \"=\" | \">=\"\ntype Agg = \"sum\" | \"max\"\nrecord R:\n  agg?: Agg\n  maybe?: \"x\"?\n  caret?: string @pattern(\"^[a-z]+$\\\\-\")\n  slug: string @len(..8) @pattern(\"[a-z0-9]([a-z0-9-]*[a-z0-9])?\")\n  dot?: string @pattern(\"a.c[.]\")\n  op?: Op\n  one?: \"x\"\n",
    )
    .unwrap();
    let new = assert_round_trip(&old);
    assert_eq!(new.fingerprint(), old.fingerprint());
}

#[test]
fn classes_round_trip_exactly() {
    let old = parse_schema(
        "\
@schema classes@1.0.0
@root User

type Token = string @secret

record Address @personal:
  street: string

enum Role @secret:
  admin
  user

record User:
  name: string @personal
  nick: string? @personal
  password: Token
  blob: any @secret
  address: Address?
  role: Role
",
    )
    .unwrap();
    let new = assert_round_trip(&old);
    assert_eq!(new.to_string(), old.to_string());

    let json = to_json_schema(&old);
    let defs = &json["$defs"];
    assert_eq!(
        defs["Token"],
        serde_json::json!({"type": "string", "writeOnly": true, "x-nova-class": "secret"})
    );
    assert_eq!(defs["Address"]["x-nova-class"], "personal");
    assert_eq!(defs["Address"].get("writeOnly"), None);
    assert_eq!(defs["Role"]["x-nova-class"], "secret");
    assert_eq!(defs["Role"]["writeOnly"], true);
    let user = &defs["User"]["properties"];
    assert_eq!(user["name"], serde_json::json!({"type": "string", "x-nova-class": "personal"}));
    assert_eq!(user["nick"], serde_json::json!({"type": ["string", "null"], "x-nova-class": "personal"}));
    assert_eq!(user["blob"], serde_json::json!({"writeOnly": true, "x-nova-class": "secret"}));
}

/// The standard library schema as `parse_schema` links it, for its fingerprint.
fn std_import() -> String {
    let std = novafile_schema::std_schemas().last().unwrap();
    format!("@import {}#{} as std", std.reference().unwrap(), std.fingerprint_hex())
}

#[test]
fn std_references_round_trip_exactly() {
    let old = parse_schema(&format!(
        "\
@schema people@1.0.0
{}
@root Person

record Person:
  id: std.Uuid
  email: std.Email @personal
  home?: std.Uri?
  port: std.Port
",
        std_import()
    ))
    .unwrap();
    let new = assert_round_trip(&old);
    assert_eq!(new.to_string(), old.to_string());
    assert_eq!(new.fingerprint(), old.fingerprint());

    let json = to_json_schema(&old);
    let defs = json["$defs"].as_object().unwrap();
    // Only the imported definitions in use are written.
    let mut names: Vec<&str> = defs.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["Person", "std.Email", "std.Port", "std.Uri", "std.Uuid"]);
    let uuid = &defs["std.Uuid"];
    assert_eq!(uuid["format"], "uuid");
    assert_eq!(uuid["x-nova-type"], "novafile.dev/std.Uuid");
    assert_eq!(uuid["pattern"], "^(?:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$");
    assert_eq!(defs["std.Email"]["format"], "email");
    assert_eq!(defs["std.Uri"]["format"], "uri");
    assert_eq!(defs["std.Port"]["x-nova-type"], "novafile.dev/std.Port");
    assert_eq!(defs["std.Port"].get("format"), None);
    let person = &defs["Person"]["properties"];
    assert_eq!(person["id"], serde_json::json!({"$ref": "#/$defs/std.Uuid"}));
    assert_eq!(person["email"], serde_json::json!({"$ref": "#/$defs/std.Email", "x-nova-class": "personal"}));
}

#[test]
fn imported_definitions_are_written_transitively() {
    let common = parse_schema("@schema acme/common@1.0.0\n\ntype Id = string @len(1..10)\n\nrecord Ref:\n  id: Id\n\nrecord Unused:\n  x: int\n").unwrap();
    let mut main =
        novafile_schema::parse_schema_unlinked("@schema acme/main@1.0.0\n@import acme/common@1.0.0 as c\n@root Main\n\nrecord Main:\n  r: c.Ref\n").unwrap();
    main.link(&|_| Ok(common.clone())).unwrap();
    let json = to_json_schema(&main);
    let defs = json["$defs"].as_object().unwrap();
    let mut names: Vec<&str> = defs.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["Main", "c.Id", "c.Ref"]);
    assert_eq!(defs["c.Ref"]["properties"]["id"], serde_json::json!({"$ref": "#/$defs/c.Id"}));
    assert_eq!(defs["c.Id"].get("x-nova-type"), None);
}

/// Integer bounds beyond 64 bits round outward in JSON and come back exact.
#[test]
fn wide_integer_bounds_round_outward_and_back() {
    let old = parse_schema("@root R\n\nrecord R:\n  n: int @range(-18446744073709551617..18446744073709551617)\n").unwrap();
    let json = to_json_schema(&old);
    let n = &json["$defs"]["R"]["properties"]["n"];
    let num = |k: &str| n[k].as_f64().unwrap();
    // 2^64 + 1 must stay inside the JSON bounds.
    assert!(num("maximum") > 18_446_744_073_709_551_616.0, "{n}");
    assert!(num("minimum") < -18_446_744_073_709_551_616.0, "{n}");
    assert_eq!(n["x-nova-range"], "-18446744073709551617..18446744073709551617", "{n}");
    let new = assert_round_trip(&old);
    assert_eq!(new.to_string(), old.to_string());
}

/// Documentation becomes `description` and comes back from it (#34).
#[test]
fn docs_round_trip_through_description() {
    let s =
        parse_schema("@schema docs@1.0.0\n@root Db\n\n# The database.\nrecord Db:\n  # Its URL.\n  url: string\n  # The key.\n  id: string @key\n").unwrap();
    let j = to_json_schema(&s);
    assert_eq!(j["$defs"]["Db"]["description"], "The database.");
    assert_eq!(j["$defs"]["Db"]["properties"]["url"]["description"], "Its URL.");
    assert!(
        j["$defs"]["Db"]["properties"]["id"]["description"]
            .as_str()
            .unwrap()
            .starts_with("The key.\n\nKey field (@key)")
    );
    let back = from_json_schema(&j, None, None).unwrap().schema;
    assert_eq!(back.to_string(), s.to_string());
}
