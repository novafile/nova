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
    assert_eq!(json["$id"], "urn:nova:infrastructure@1.0.0");
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
