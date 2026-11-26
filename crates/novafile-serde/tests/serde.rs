//! Serde round trips through NOVA text, binary and `Value`, including error paths and JSON interop.

use novafile_serde::{Profile, from_slice, from_str, from_value, to_string, to_string_with, to_value, to_vec};
use novafile_value::{Decimal, Rational, Timestamp, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
enum Status {
    Active,
    Failed(String),
    Moved(String, u16),
    Scaled { from: u16, to: u16 },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct Service {
    id: String,
    port: u16,
    replicas: u16,
    #[serde(default)]
    enabled: bool,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(with = "novafile_serde::with::set", default)]
    tags: BTreeSet<String>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct Infra {
    environment: String,
    deployed_at: Timestamp,
    price: Decimal,
    ratio: Rational,
    #[serde(with = "novafile_serde::with::duration")]
    timeout: std::time::Duration,
    services: Vec<Service>,
    by_port: BTreeMap<u16, String>,
    labels: HashMap<String, String>,
    pair: (i32, String),
    big: i128,
    letter: char,
    unit: (),
    extra: Value,
}

fn sample() -> Infra {
    Infra {
        environment: "production".into(),
        deployed_at: Timestamp::parse("2026-11-17T16:00:00Z").unwrap(),
        price: Decimal::parse("12.50").unwrap(),
        ratio: Rational::parse("22/7").unwrap(),
        timeout: std::time::Duration::from_millis(1500),
        services: vec![
            Service {
                id: "auth".into(),
                port: 8080,
                replicas: 3,
                enabled: true,
                status: Status::Active,
                owner: None,
                tags: BTreeSet::new(),
            },
            Service {
                id: "api".into(),
                port: 3000,
                replicas: 5,
                enabled: true,
                status: Status::Scaled { from: 3, to: 5 },
                owner: Some("ops".into()),
                tags: ["edge".to_string(), "public".to_string()].into(),
            },
            Service {
                id: "old".into(),
                port: 1,
                replicas: 0,
                enabled: false,
                status: Status::Moved("eu".into(), 2),
                owner: None,
                tags: BTreeSet::new(),
            },
            Service {
                id: "bad".into(),
                port: 2,
                replicas: 0,
                enabled: false,
                status: Status::Failed("oom".into()),
                owner: None,
                tags: BTreeSet::new(),
            },
        ],
        by_port: [(8080, "auth".into()), (3000, "api".into())].into(),
        labels: [("team".to_string(), "core".to_string())].into(),
        pair: (-1, "x".into()),
        big: -(1i128 << 100),
        letter: 'λ',
        unit: (),
        extra: novafile_text::parse("when: @timestamp(\"2026-01-01T00:00:00Z\")\nset: @set[1, 2]\nv: !on\nm: @map{1 => one}\n").unwrap(),
    }
}

#[test]
fn round_trips_text_binary_and_value() {
    let s = sample();
    for p in [Profile::Human, Profile::Compact, Profile::Canonical] {
        let t = to_string_with(&s, p).unwrap();
        assert_eq!(from_str::<Infra>(&t).unwrap_or_else(|e| panic!("{e}\n{t}")), s, "{t}");
    }

    assert_eq!(from_slice::<Infra>(&to_vec(&s).unwrap()).unwrap(), s);
    assert_eq!(from_value::<Infra>(to_value(&s).unwrap()).unwrap(), s);
}

#[test]
fn uses_rich_nova_kinds() {
    let v = to_value(&sample()).unwrap();
    let at = |p: &str| novafile_text::Path::parse(p).unwrap().get(&v).unwrap().clone();
    assert!(matches!(at("deployed_at"), Value::Timestamp(_)));
    assert!(matches!(at("price"), Value::Decimal(_)));
    assert!(matches!(at("ratio"), Value::Rational(_)));
    assert_eq!(at("timeout").kind(), "duration");
    assert_eq!(at("services[id=api].tags").kind(), "set");
    assert_eq!(at("services[id=api].status"), novafile_text::parse_flow("!Scaled({from: 3, to: 5})").unwrap());
    assert_eq!(at("services[id=old].status"), novafile_text::parse_flow("!Moved((eu, 2))").unwrap());
    assert_eq!(at("by_port").kind(), "map");
    assert_eq!(at("labels").kind(), "record");
    assert_eq!(at("pair").kind(), "tuple");
    // A Value field passes through untouched, including kinds serde has no notion of.
    assert_eq!(at("extra.set").kind(), "set");
    assert_eq!(at("extra.v"), Value::Variant("on".into(), None));
    let text = to_string(&sample()).unwrap();
    assert!(
        text.contains("deployed_at: @timestamp(\"2026-11-17T16:00:00Z\")") && text.contains("status: !Active"),
        "{text}"
    );
}

#[test]
fn the_rust_type_acts_as_a_schema() {
    // Plain strings in a schema-free document resolve into typed fields.
    #[derive(Deserialize, Debug)]
    struct T {
        at: Timestamp,
        price: Decimal,
        status: Status,
        ratio: Rational,
    }

    let t: T = from_str("at: 2026-11-17T16:00:00Z\nprice: 12.50\nstatus: Active\nratio: 3\n").unwrap();
    assert_eq!(t.at.to_string(), "2026-11-17T16:00:00Z");
    assert_eq!(t.price.to_string(), "12.5");
    assert_eq!(t.status, Status::Active);
    assert_eq!(t.ratio.to_string(), "3/1");
}

#[test]
fn errors_carry_paths() {
    #[derive(Serialize)]
    struct D {
        #[serde(with = "novafile_serde::with::set")]
        tags: Vec<&'static str>,
    }

    let text = to_string(&sample()).unwrap().replace("port: 3000", "port: 70000");
    let e = from_str::<Infra>(&text).unwrap_err();
    assert_eq!(e.path(), "services[1].port");
    assert!(
        e.to_string().starts_with("services[1].port: invalid value: integer `70000`, expected u16"),
        "{e}"
    );

    let e = from_str::<Infra>(&to_string(&sample()).unwrap().replace("!Active", "!Exploded")).unwrap_err();
    assert!(e.to_string().contains("unknown variant `Exploded`"), "{e}");

    assert!(to_value(&D { tags: vec!["a", "a"] }).unwrap_err().to_string().contains("duplicate set element"));
}

#[test]
fn json_interop_through_serde() {
    // NOVA values serialize naturally with other serde formats…
    let v = novafile_text::parse("a: 1\nt: @timestamp(\"2026-11-17T16:00:00Z\")\nr: 22/7\nv: !failed(oom)\ns: @set[x]\n").unwrap();
    let j = serde_json::to_string(&v).unwrap();
    assert_eq!(j, r#"{"a":1,"t":"2026-11-17T16:00:00Z","r":"22/7","v":{"failed":"oom"},"s":["x"]}"#);
    // …and data that came through JSON still deserializes into rich Rust types.
    let s = sample();
    let json = serde_json::to_string(&s).unwrap();
    let back: Infra = serde_json::from_str(&json).unwrap();
    assert_eq!(back.deployed_at, s.deployed_at);
    assert_eq!(back.services[1].status, s.services[1].status);
    // JSON objects with numeric keys map back into integer-keyed maps.
    let from_json = novafile_json::from_json_str(&json).unwrap();
    assert_eq!(from_value::<Infra>(from_json).unwrap().by_port, s.by_port);
}

#[test]
fn internally_tagged_and_untagged_enums() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(tag = "kind")]
    enum Shape {
        Circle { r: f64 },
        Square { side: f64 },
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(untagged)]
    enum Num {
        I(i64),
        S(String),
    }

    let shapes = vec![Shape::Circle { r: 1.5 }, Shape::Square { side: 2.0 }];
    let t = to_string_with(&shapes, Profile::Compact).unwrap();
    assert_eq!(t, "[2]{kind,r,side}:\n  Circle,1.5,\n  Square,,2.0\n");
    assert_eq!(from_str::<Vec<Shape>>(&t).unwrap(), shapes);
    assert_eq!(from_str::<Vec<Num>>("- 1\n- x\n").unwrap(), vec![Num::I(1), Num::S("x".into())]);
}

#[test]
fn f32_is_written_in_its_shortest_form() {
    assert_eq!(novafile_serde::to_string(&4.55f32).unwrap().trim(), "4.55");
    let v: f32 = novafile_serde::from_str(&novafile_serde::to_string(&0.1f32).unwrap()).unwrap();
    assert_eq!(v, 0.1f32);
}

/// A finite float beyond the f32 range is an error, not infinity.
#[test]
fn f32_overflow_is_an_error() {
    assert!(from_value::<f32>(Value::Float(1e300)).is_err());
    assert_eq!(from_value::<f32>(Value::Float(1.5)).unwrap(), 1.5);
    assert!(from_value::<f32>(Value::Float(f64::INFINITY)).unwrap().is_infinite());
}

/// Variant tags must be identifiers (§2.11); a rename to anything else is an error.
#[test]
fn non_identifier_variant_tags_are_errors() {
    #[derive(Serialize)]
    enum Op {
        #[serde(rename = "at start")]
        AtStart,
        #[serde(rename = "at-end")]
        AtEnd(u8),
    }

    assert!(to_value(&Op::AtStart).unwrap_err().to_string().contains("not an identifier"));
    assert_eq!(to_value(&Op::AtEnd(1)).unwrap(), Value::Variant("at-end".into(), Some(Box::new(Value::Int(1)))));
}

/// `from_str_with_schema` checks the document's `@schema` line against a named schema (#28).
#[cfg(feature = "schema")]
#[test]
fn with_schema_checks_the_schema_reference() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Settings {
        name: Option<String>,
    }

    let named = novafile_schema::parse_schema("@schema example.org/app@2.1.0\n@root Settings\nrecord Settings:\n  name?: string\n").unwrap();
    let load = |doc: &str| novafile_serde::from_str_with_schema::<Settings>(doc, &named);

    let e = load("@schema example.org/other@1.0.0\nname: x\n").unwrap_err().to_string();
    assert!(e.contains("example.org/other") && e.contains("example.org/app"), "{e}");
    let e = load("@schema example.org/app@2.2.0\nname: x\n").unwrap_err().to_string();
    assert!(e.contains("cannot read"), "{e}");
    assert!(
        load("@schema example.org/app@1.0.0\nname: x\n")
            .unwrap_err()
            .to_string()
            .contains("cannot read")
    );
    load("@schema example.org/app@2.0.0\nname: x\n").unwrap();
    load("name: x\n").unwrap();

    // An anonymous schema has nothing to compare the reference with.
    let anonymous = novafile_schema::parse_schema("@root Settings\nrecord Settings:\n  name?: string\n").unwrap();
    novafile_serde::from_str_with_schema::<Settings>("@schema example.org/other@1.0.0\nname: x\n", &anonymous).unwrap();
}

/// Layered configuration (spec §6.4): each layer passes the partial check, the layers merge
/// with `Value::merge`, and only the merged value gets defaults, required fields and rules.
#[test]
fn layered_configuration_merges_then_resolves() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct Config {
        name: String,
        port: u16,
        tags: Vec<String>,
        db: Db,
    }
    #[derive(Deserialize, Debug, PartialEq)]
    struct Db {
        url: Option<String>,
        url_file: Option<String>,
        pool: u16,
    }

    let schema = novafile_schema::parse_schema(
        "@root Config\nrecord Config:\n  name: string\n  port: u16\n  tags: string[]\n  db: Db\n\
         record Db @one_of(url, url_file):\n  url?: string\n  url_file?: string\n  pool: u16 = 10\n",
    )
    .unwrap();
    let parse = |src: &str| novafile_text::parse(src).unwrap();
    let merged = |layers: &[&str]| {
        let mut v = Value::Record(novafile_value::Record::new());
        for src in layers {
            let layer = parse(src);
            schema.resolve_partial(&layer).unwrap();
            v.merge(layer);
        }
        schema.resolve(&v)
    };

    let base = "name: app\ntags: [a, b]\ndb:\n  url: \"postgres://base\"\n";
    let prod = "port: 8080\ntags: [c]\n";
    // A single layer is incomplete: valid partially, missing `port` in full.
    assert!(schema.resolve(&parse(base)).is_err());

    // The upper layer wins key by key, lists replace, and `pool` gets its default only now.
    let config: Config = from_value(merged(&[base, prod]).unwrap()).unwrap();
    assert_eq!(
        config,
        Config {
            name: "app".into(),
            port: 8080,
            tags: vec!["c".into()],
            db: Db {
                url: Some("postgres://base".into()),
                url_file: None,
                pool: 10
            },
        }
    );

    // Each layer satisfies `@one_of` alone; the merge keeps both fields and breaks it.
    let issues = merged(&[base, prod, "db:\n  url_file: /run/url\n"]).unwrap_err();
    assert_eq!(issues.len(), 1);
    assert!(issues[0].to_string().contains("@one_of(url, url_file): 2 present"), "{}", issues[0]);
}
