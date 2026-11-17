//! `to_json_schema` describes what `novafile-json` writes for resolved documents:
//! checked with an independent JSON Schema validator.

use novafile_schema::{parse_schema, Schema};
use novafile_value::Value;

mod common;
use common::{example, KITCHEN_SINK};

fn validator(schema: &Schema) -> jsonschema::Validator {
    let json = novafile_schemars::to_json_schema(schema);
    assert!(jsonschema::meta::is_valid(&json), "not a valid JSON Schema: {json:#}");
    jsonschema::validator_for(&json).unwrap()
}

/// The document as JSON, the way `nvf convert --to json` writes it.
fn json_of(v: &Value) -> serde_json::Value {
    serde_json::from_str(&novafile_json::to_json_string(v, false).0).unwrap()
}

/// A change to a JSON document.
type Edit = dyn Fn(&mut serde_json::Value);

fn errors(validator: &jsonschema::Validator, instance: &serde_json::Value) -> Vec<String> {
    validator.iter_errors(instance).map(|e| format!("{}: {e}", e.instance_path())).collect()
}

#[test]
fn infrastructure_documents() {
    let schema = parse_schema(&example("infrastructure.novs")).unwrap();
    let v = validator(&schema);
    let doc = novafile_text::parse(&example("infrastructure.nova")).unwrap();
    let resolved = schema.resolve(&doc).unwrap();
    let good = json_of(&resolved);
    assert_eq!(errors(&v, &good), Vec::<String>::new());
    // The unresolved document is valid too: defaults may be absent, and
    // timestamps and sets are already strings and arrays.
    assert_eq!(errors(&v, &json_of(&doc)), Vec::<String>::new());

    let bad = |edit: &Edit| {
        let mut j = good.clone();
        edit(&mut j);
        let e = errors(&v, &j);
        // NOVA agrees that the document is invalid.
        let nova = novafile_json::from_json_str(&j.to_string()).unwrap();
        assert!(schema.resolve(&nova).is_err(), "NOVA accepted {j}");
        e
    };
    let e = bad(&|j| j["services"][0]["port"] = 70000.into());
    assert!(
        e.len() == 1 && e[0].starts_with("/services/0/port: 70000 is greater than the maximum of 65535"),
        "{e:?}"
    );
    let e = bad(&|j| j["services"][0]["port"] = 0.into());
    assert!(e.len() == 1 && e[0].starts_with("/services/0/port: 0 is less than the minimum of 1"), "{e:?}");
    let e = bad(&|j| j["services"][1]["replicas"] = 101.into());
    assert_eq!(e.len(), 1, "{e:?}");
    let e = bad(&|j| j["services"][0]["prot"] = 1.into());
    assert!(
        e.len() == 1 && e[0].contains("Additional properties are not allowed ('prot' was unexpected)"),
        "{e:?}"
    );
    let e = bad(&|j| j["status"] = "exploded".into());
    assert!(e.len() == 1 && e[0].starts_with("/status: "), "{e:?}");
    let e = bad(&|j| j["status"] = serde_json::json!({"active": "x"}));
    assert_eq!(e.len(), 1, "{e:?}");
    let e = bad(&|j| j["services"][1]["tags"] = serde_json::json!(["a", "a"]));
    assert!(e.len() == 1 && e[0].contains("non-unique"), "{e:?}");
    let e = bad(&|j| j["services"][0]["image"] = "".into());
    assert_eq!(e.len(), 1, "{e:?}");
    let e = bad(&|j| {
        j.as_object_mut().unwrap().remove("region");
    });
    assert!(e.len() == 1 && e[0].contains("\"region\" is a required property"), "{e:?}");
    let e = bad(&|j| j["deployed_at"] = 5.into());
    assert_eq!(e.len(), 1, "{e:?}");

    // A payload variant validates (but resolution of `{tag: payload}` from
    // JSON is not part of spec §6.3, so NOVA itself needs the variant).
    let mut j = good.clone();
    j["status"] = serde_json::json!({"failed": "disk full"});
    assert_eq!(errors(&v, &j), Vec::<String>::new());
}

const KITCHEN_DOC: &str = r#"
id: k1
any_value: {anything: [1, 2.5, "x"]}
nothing: null
big: -5
i8: -128
i16: 100
i32: 2147483647
i64: -9223372036854775808
u8: 255
u16: 443
u32: 4294967295
u64: 18446744073709551615
ratio: 0.25
price: @decimal("1000.5")
share: 1/3
name: héllo
blob: @bytes("AP8H")
ttl: @duration("PT90M")
tags: [b, a]
points: [(1.5, -2.0), (@float("nan"), @float("-inf"))]
labels: {a: 1, b: -2}
by_id: @map{1 => one, 2 => two}
nested: [{p: {x: 1.0, y: 2.0, extra: true}, q: null}, {}]
maybe: null
status: !failed({x: 0.0, y: 1.0})
later: 2026-11-17T16:00:00.123+02:00
"#;

#[test]
fn every_kind_validates() {
    let schema = parse_schema(KITCHEN_SINK).unwrap();
    let v = validator(&schema);
    let doc = novafile_text::parse(KITCHEN_DOC).unwrap();
    let resolved = schema.resolve(&doc).unwrap_or_else(|e| panic!("{e:#?}"));
    let good = json_of(&resolved);
    assert_eq!(errors(&v, &good), Vec::<String>::new(), "{good:#}");
    // Resolution turned the `labels` record into a map, written as pairs.
    assert_eq!(good["labels"], serde_json::json!([["a", 1], ["b", -2]]));
    let ok = |field: &str, value: &str| {
        let mut j = good.clone();
        j[field] = serde_json::from_str(value).unwrap();
        assert_eq!(errors(&v, &j), Vec::<String>::new(), "{field}: {value}");
    };
    ok("status", r#""idle""#);
    ok("status", r#"{"busy": 3}"#);
    ok("status", r#""stopped""#);
    // The record form of a string-keyed map, as serde_json and unresolved documents write it.
    ok("labels", r#"{"a": 1, "b": -2}"#);
    ok("nested", r#"[{"p": {"x": 1.0, "y": 2.0}, "q": null}]"#);
    // Unresolved, the document validates too: fields with defaults may be
    // absent, and the map is still a record (an object).
    assert_eq!(errors(&v, &json_of(&doc)), Vec::<String>::new());

    let bad = |edit: &Edit| {
        let mut j = good.clone();
        edit(&mut j);
        errors(&v, &j)
    };
    let cases: Vec<(&str, Box<Edit>)> = vec![
        ("i8 range", Box::new(|j| j["i8"] = (-129).into())),
        ("@range on i16", Box::new(|j| j["i16"] = 101.into())),
        ("u64 sign", Box::new(|j| j["u64"] = (-1).into())),
        ("int range", Box::new(|j| j["big"] = (-6).into())),
        ("float range", Box::new(|j| j["ratio"] = 1.5.into())),
        ("float as other string", Box::new(|j| j["ratio"] = "1.5".into())),
        ("decimal syntax", Box::new(|j| j["price"] = "1,5".into())),
        ("decimal as number", Box::new(|j| j["price"] = 1.5.into())),
        ("rational syntax", Box::new(|j| j["share"] = "1/0".into())),
        ("duration syntax", Box::new(|j| j["ttl"] = "90 minutes".into())),
        ("tuple length", Box::new(|j| j["points"][0] = serde_json::json!([1.0, 2.0, 3.0]))),
        ("set length", Box::new(|j| j["tags"] = serde_json::json!([]))),
        ("map value (pairs)", Box::new(|j| j["labels"][0][1] = "x".into())),
        ("map value (object)", Box::new(|j| j["labels"] = serde_json::json!({"a": "x"}))),
        ("map pair shape", Box::new(|j| j["labels"][0] = serde_json::json!(["a"]))),
        ("int-keyed map pair", Box::new(|j| j["by_id"][0] = serde_json::json!(["1", "one"]))),
        ("payload range", Box::new(|j| j["status"] = serde_json::json!({"busy": 300}))),
        ("unknown tag", Box::new(|j| j["status"] = serde_json::json!({"paused": 1}))),
        ("unit with payload", Box::new(|j| j["status"] = serde_json::json!({"idle": 1}))),
        ("closed record", Box::new(|j| j["surprise"] = 1.into())),
        ("open record still typed", Box::new(|j| j["maybe"] = serde_json::json!({"x": "a", "y": 1.0}))),
        ("string length", Box::new(|j| j["id"] = "".into())),
    ];
    for (what, edit) in cases {
        assert!(!bad(&*edit).is_empty(), "{what}: expected a validation error");
    }
    // Documented limits: constraints JSON Schema cannot express are not checked.
    assert!(bad(&|j| j["price"] = "2000".into()).is_empty(), "decimal @range is x-nova-range only");
    assert!(bad(&|j| j["name"] = "x".repeat(11).into()).is_empty(), "@len(.., utf8) is x-nova-len only");
}
