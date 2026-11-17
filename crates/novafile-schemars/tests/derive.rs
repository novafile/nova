//! `#[derive(JsonSchema)]` → NOVA schema → validate real serde values.

use novafile_value::{Decimal, Duration, Timestamp, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Serialize, Deserialize, JsonSchema, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Active,
    Draining,
    Failed(String),
    Moved { region: String, at: Timestamp },
    Resized(u8, u8),
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, PartialEq)]
struct Limits {
    cpu: Decimal,
    memory_mb: u32,
    timeout: Option<Duration>,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct Service {
    id: String,
    port: u16,
    #[serde(rename = "replicaCount")]
    replicas: u8,
    #[serde(default)]
    enabled: bool,
    tags: HashSet<String>,
    labels: BTreeMap<String, String>,
    endpoints: Vec<(String, u16)>,
    limits: Option<Limits>,
    status: Status,
    offset: i64,
    deployed_at: Timestamp,
}

#[derive(Serialize, Deserialize, JsonSchema, Debug, PartialEq)]
struct Fleet {
    name: String,
    services: Vec<Service>,
    weights: BTreeMap<String, f64>,
    extra: Value,
}

const FLEET_NOVS: &str = "\
@schema fleet@1.2.0
@root Fleet

record Fleet:
  name: string
  services: list<Service>
  weights: map<string, float>
  extra: any

record Limits:
  cpu: decimal
  memory_mb: u32
  timeout?: duration?

record Service:
  id: string
  port: u16
  replicaCount: u8
  tags: set<string>
  labels: map<string, string>
  endpoints: list<(string, u16)>
  status: Status
  offset: i64
  deployed_at: timestamp
  enabled: bool = false
  limits?: Limits?

enum Status:
  active
  draining
  failed(string)
  moved(StatusMoved)
  resized((u8, u8))

record StatusMoved:
  region: string
  at: timestamp
";

fn service(id: &str, status: Status) -> Service {
    Service {
        id: id.into(),
        port: 8080,
        replicas: 3,
        enabled: true,
        tags: ["web".to_string(), "edge".to_string()].into_iter().collect(),
        labels: [("team".to_string(), "infra".to_string())].into_iter().collect(),
        endpoints: vec![("http".into(), 80), ("https".into(), 443)],
        limits: Some(Limits {
            cpu: Decimal::parse("1.5").unwrap(),
            memory_mb: 512,
            timeout: Some(Duration::parse("30s").unwrap()),
        }),
        status,
        offset: -42,
        deployed_at: Timestamp::parse("2026-11-17T16:00:00Z").unwrap(),
    }
}

fn fleet() -> Fleet {
    Fleet {
        name: "prod".into(),
        services: vec![
            service("api", Status::Active),
            service("db", Status::Failed("disk full".into())),
            service(
                "cache",
                Status::Moved {
                    region: "eu".into(),
                    at: Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
                },
            ),
            service("queue", Status::Resized(2, 4)),
        ],
        weights: [("api".to_string(), 0.5), ("db".to_string(), 1.0)].into_iter().collect(),
        extra: Value::List(vec![Value::Int(1), Value::String("x".into())]),
    }
}

#[test]
fn derived_schema_renders_as_expected() {
    let c = novafile_schemars::conversion_for::<Fleet>("fleet", "1.2.0").unwrap();
    assert_eq!(c.schema.to_string(), FLEET_NOVS);
    assert!(c.warnings.is_empty(), "{:#?}", c.warnings);
    // The rendered text is valid schema language and re-renders identically.
    assert_eq!(novafile_schema::parse_schema(FLEET_NOVS).unwrap(), c.schema);
}

#[test]
fn serialized_values_resolve() {
    let schema = novafile_schemars::schema_for::<Fleet>("fleet", "1.2.0").unwrap();
    let f = fleet();
    let v = novafile_serde::to_value(&f).unwrap();
    let resolved = schema.resolve(&v).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(novafile_serde::from_value::<Fleet>(resolved).unwrap(), f);

    // Through NOVA text, too.
    let text = novafile_serde::to_string(&f).unwrap();
    let parsed = novafile_text::parse(&text).unwrap();
    schema.resolve(&parsed).unwrap_or_else(|e| panic!("{e:?}"));

    // A missing `#[serde(default)]` field takes its default during resolution.
    let mut v = novafile_serde::to_value(&service("x", Status::Active)).unwrap();
    if let Value::Record(r) = &mut v {
        r.remove("enabled");
    }
    let s = schema.resolve_as(&v, &novafile_schema::parse_type("Service").unwrap()).unwrap();
    assert_eq!(s.as_record().unwrap().get("enabled"), Some(&Value::Bool(false)));
}

/// JSON produced by `serde_json` for the same types, read back with
/// `novafile-json`: unit variants, timestamps, decimals and sets resolve from
/// their JSON spellings.
fn json_fleet(edit: impl FnOnce(&mut serde_json::Value)) -> Value {
    let mut f = fleet();
    // Variants with payloads arrive from JSON as `{tag: payload}` records,
    // which schema resolution (spec §6.3) does not convert; keep unit variants.
    for s in &mut f.services {
        s.status = Status::Draining;
    }
    let mut j = serde_json::to_value(&f).unwrap();
    edit(&mut j);
    novafile_json::from_json_str(&j.to_string()).unwrap()
}

fn issues(v: &Value) -> Vec<String> {
    let schema = novafile_schemars::schema_for::<Fleet>("fleet", "1.2.0").unwrap();
    match schema.resolve(v) {
        Ok(_) => Vec::new(),
        Err(issues) => issues.iter().map(|i| i.to_string()).collect(),
    }
}

#[test]
fn json_instances_resolve_and_violations_are_reported() {
    assert_eq!(issues(&json_fleet(|_| {})), Vec::<String>::new());

    let v = json_fleet(|j| j["services"][1]["port"] = 70000.into());
    assert_eq!(issues(&v), ["services[1].port: 70000 is out of range for u16"]);

    let v = json_fleet(|j| j["services"][0]["prot"] = 1.into());
    assert_eq!(issues(&v), ["services[0].prot: unknown field"]);

    let v = json_fleet(|j| j["services"][2]["status"] = "exploded".into());
    let i = issues(&v);
    assert!(i.len() == 1 && i[0].starts_with("services[2].status: unknown variant \"exploded\""), "{i:?}");

    let v = json_fleet(|j| j["services"][0]["limits"]["cpu"] = "1.5.1".into());
    assert!(issues(&v)[0].starts_with("services[0].limits.cpu: string is not a valid decimal"));

    let v = json_fleet(|j| j["services"][0]["tags"] = serde_json::json!(["a", "a"]));
    assert_eq!(issues(&v).len(), 1);

    let v = json_fleet(|j| j["services"][0]["endpoints"][0] = serde_json::json!(["http"]));
    assert!(issues(&v)[0].contains("expected a 2-tuple"));
}

#[test]
fn serde_attributes_and_open_records() {
    /// Not `deny_unknown_fields`, but `additionalProperties: true` via schemars.
    #[derive(JsonSchema)]
    #[schemars(extend("additionalProperties" = true))]
    #[allow(dead_code)]
    struct Loose {
        #[serde(rename = "a-b")]
        ab: u64,
        #[serde(skip)]
        skipped: u8,
        #[deprecated]
        old: Option<i8>,
    }
    let schema = novafile_schemars::schema_for::<Loose>("loose", "0.1.0").unwrap();
    assert_eq!(
        schema.to_string(),
        "@schema loose@0.1.0\n@root Loose\n\nrecord Loose @open:\n  a-b: u64\n  old?: i8? @deprecated\n"
    );

    // Internally tagged and untagged enums are approximated, with warnings.
    #[derive(JsonSchema)]
    #[serde(tag = "kind")]
    #[allow(dead_code)]
    enum Shape {
        Circle { r: f64 },
        Square { side: f64 },
    }
    #[derive(JsonSchema)]
    #[serde(untagged)]
    #[allow(dead_code)]
    enum Either {
        N(u8),
        S(String),
    }
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Holder {
        shape: Shape,
        either: Either,
    }
    let c = novafile_schemars::conversion_for::<Holder>("holder", "1.0.0").unwrap();
    assert_eq!(
        c.schema.to_string(),
        "@schema holder@1.0.0\n@root Holder\n\ntype Either = any\n\nrecord Holder:\n  shape: Shape\n  either: Either\n\nrecord Shape:\n  kind: string\n  r?: float\n  side?: float\n"
    );
    assert_eq!(
        c.warnings,
        [
            "#/$defs/Either: anyOf that is not a serde externally tagged enum (e.g. an untagged enum) mapped to any",
            "#/$defs/Shape: internally/adjacently tagged union (tag \"kind\": Circle | Square) mapped to a record whose other fields are optional",
        ]
    );
}

#[test]
fn invalid_names_and_versions_are_errors() {
    assert!(matches!(
        novafile_schemars::schema_for::<Limits>("Bad Name", "1.0.0"),
        Err(novafile_schemars::Error::InvalidName(_))
    ));
    assert!(matches!(
        novafile_schemars::schema_for::<Limits>("ok", "v1"),
        Err(novafile_schemars::Error::InvalidVersion(_))
    ));
}
