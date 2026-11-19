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
