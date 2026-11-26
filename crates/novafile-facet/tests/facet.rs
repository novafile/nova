//! Integration tests for the facet binding: round trips, serde parity and
//! error paths.

use facet::Facet;
use novafile_facet::{Profile, from_slice, from_str, from_value, to_string, to_string_with, to_value, to_vec};
use novafile_value::{Decimal, Duration, Rational, Timestamp, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

// The sample types derive both Facet and serde, so the two bindings can be
// checked against each other. serde needs `with` helpers for sets and
// `std::time::Duration`; facet knows both natively.

#[derive(Facet, Serialize, Deserialize, Debug, PartialEq, Clone)]
#[repr(u8)]
enum Status {
    Active,
    Failed(String),
    Moved(String, u16),
    Scaled { from: u16, to: u16 },
}

#[derive(Facet, Serialize, Deserialize, Debug, PartialEq, Clone)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "the unsafe code is in the Facet derive's vtable, which is sound for any field values"
)]
struct Service {
    id: String,
    port: u16,
    replicas: u16,
    #[serde(default)]
    #[facet(default)]
    enabled: bool,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[facet(skip_serializing_if = Option::is_none)]
    owner: Option<String>,
    #[serde(with = "novafile_serde::with::set", default)]
    #[facet(default)]
    tags: BTreeSet<String>,
}

#[derive(Facet, Serialize, Deserialize, Debug, PartialEq, Clone)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "the unsafe code is in the Facet derive's vtable, which is sound for any field values"
)]
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

fn flow(s: &str) -> Value {
    novafile_text::parse_flow(s).unwrap()
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
    assert_eq!(at("timeout"), Value::Duration(Duration::parse("1.5s").unwrap()));
    // Sets are native in facet: no `with` helper needed.
    assert_eq!(at("services[id=api].tags").kind(), "set");
    assert_eq!(at("services[id=api].status"), flow("!Scaled({from: 3, to: 5})"));
    assert_eq!(at("services[id=old].status"), flow("!Moved((eu, 2))"));
    assert_eq!(at("services[id=bad].status"), flow("!Failed(oom)"));
    assert_eq!(at("by_port").kind(), "map");
    assert_eq!(at("labels").kind(), "record");
    assert_eq!(at("pair").kind(), "tuple");
    assert_eq!(at("unit"), Value::Null);
    assert_eq!(at("letter"), Value::String("λ".into()));
    // A Value field passes through untouched, including kinds facet has no notion of.
    assert_eq!(at("extra.set").kind(), "set");
    assert_eq!(at("extra.v"), Value::Variant("on".into(), None));
    // Field order is declaration order; skip_serializing_if drops `owner: None`.
    let Value::List(services) = at("services") else { panic!() };
    let keys: Vec<_> = services[0].as_record().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["id", "port", "replicas", "enabled", "status", "tags"]);
    let text = to_string(&sample()).unwrap();
    assert!(
        text.contains("deployed_at: @timestamp(\"2026-11-17T16:00:00Z\")") && text.contains("status: !Active"),
        "{text}"
    );
}

#[test]
fn agrees_with_nova_serde() {
    #[derive(Facet, Serialize, Debug)]
    struct Shapes {
        unit: Unit,
        newtype: Meters,
        tuple: Pair,
        #[expect(clippy::option_option, reason = "exercises a nested option on purpose")]
        opt: Option<Option<u8>>,
        nested: Vec<Vec<i64>>,
        float: f32,
        signed: i8,
        boxed: Box<Status>,
        shared: Box<str>,
        cow: Cow<'static, str>,
        array: [i32; 3],
    }

    #[derive(Facet, Serialize, Debug)]
    struct Unit;
    #[derive(Facet, Serialize, Debug)]
    struct Meters(f64);
    #[derive(Facet, Serialize, Debug)]
    struct Pair(u8, String);
    let s = sample();
    let via_facet = novafile_facet::to_value(&s).unwrap();
    let via_serde = novafile_serde::to_value(&s).unwrap();
    assert_eq!(via_facet, via_serde);
    assert_eq!(novafile_facet::to_string(&s).unwrap(), novafile_serde::to_string(&s).unwrap());
    // Each binding reads what the other wrote.
    assert_eq!(novafile_facet::from_value::<Infra>(via_serde).unwrap(), s);
    assert_eq!(novafile_serde::from_value::<Infra>(via_facet).unwrap(), s);
    let v = Shapes {
        unit: Unit,
        newtype: Meters(1.5),
        tuple: Pair(1, "a".into()),
        opt: Some(None),
        nested: vec![vec![1, -2], vec![]],
        float: 0.5,
        signed: -8,
        boxed: Box::new(Status::Failed("x".into())),
        shared: "s".into(),
        cow: Cow::Borrowed("c"),
        array: [1, 2, 3],
    };

    let mut via_serde = novafile_serde::to_value(&v).unwrap();
    // serde writes arrays as tuples; facet (and this binding) treat them as lists.
    via_serde.as_record_mut().unwrap().set("array", flow("[1, 2, 3]"));
    assert_eq!(novafile_facet::to_value(&v).unwrap(), via_serde);
}

#[test]
fn the_rust_type_acts_as_a_schema() {
    // Plain strings in a schema-free document resolve into typed fields.
    #[derive(Facet, Debug)]
    struct T {
        at: Timestamp,
        price: Decimal,
        status: Status,
        ratio: Rational,
        span: Duration,
        timeout: std::time::Duration,
    }

    let t: T = from_str("at: 2026-11-17T16:00:00Z\nprice: 12.50\nstatus: Active\nratio: 3\nspan: 90m\ntimeout: 2s\n").unwrap();
    assert_eq!(t.at.to_string(), "2026-11-17T16:00:00Z");
    assert_eq!(t.price.to_string(), "12.5");
    assert_eq!(t.status, Status::Active);
    assert_eq!(t.ratio.to_string(), "3/1");
    assert_eq!(t.span, Duration::parse("1h30m").unwrap());
    assert_eq!(t.timeout, std::time::Duration::from_secs(2));
}

#[test]
fn enums_of_all_shapes() {
    let all = vec![
        Status::Active,
        Status::Failed("oom".into()),
        Status::Moved("eu".into(), 2),
        Status::Scaled { from: 1, to: 2 },
    ];
    let v = to_value(&all).unwrap();
    assert_eq!(v, flow("[!Active, !Failed(oom), !Moved((eu, 2)), !Scaled({from: 1, to: 2})]"));
    assert_eq!(from_value::<Vec<Status>>(v).unwrap(), all);
    // JSON-style spellings: a string names a unit variant, `{Tag: payload}` any variant.
    let json_style = flow("[Active, {Failed: oom}, {Moved: [eu, 2]}, {Scaled: {from: 1, to: 2}}]");
    assert_eq!(from_value::<Vec<Status>>(json_style).unwrap(), all);
    // Result is an enum with Ok/Err variants.
    let r: Vec<Result<u8, String>> = vec![Ok(1), Err("no".into())];
    let v = to_value(&r).unwrap();
    assert_eq!(v, flow("[!Ok(1), !Err(no)]"));
    assert_eq!(from_value::<Vec<Result<u8, String>>>(v).unwrap(), r);
}

#[test]
fn errors_carry_paths() {
    let text = to_string(&sample()).unwrap().replace("port: 3000", "port: 70000");
    let e = from_str::<Infra>(&text).unwrap_err();
    assert_eq!(e.path(), "services[1].port");
    assert_eq!(e.to_string(), "services[1].port: invalid value: integer `70000`, expected u16");

    let e = from_str::<Infra>(&to_string(&sample()).unwrap().replace("!Active", "!Exploded")).unwrap_err();
    assert_eq!(e.path(), "services[0].status");
    assert!(e.to_string().contains("unknown variant `Exploded`, expected one of `Active`, `Failed`"), "{e}");

    let e = from_str::<Infra>(&to_string(&sample()).unwrap().replace("from: 3", "from: x")).unwrap_err();
    assert_eq!(e.to_string(), "services[1].status.Scaled.from: invalid type: string \"x\", expected u16");

    let e = from_str::<Infra>(&to_string(&sample()).unwrap().replace("8080 => auth", "8080 => 1")).unwrap_err();
    assert_eq!(e.path(), "by_port[8080]");

    let e = from_str::<Service>("id: x\nport: 1\nstatus: Active\n").unwrap_err();
    assert_eq!(e.to_string(), "missing field `replicas`");

    let e = from_str::<Infra>(
        &to_string(&sample())
            .unwrap()
            .replace("deployed_at: @timestamp(\"2026-11-17T16:00:00Z\")", "deployed_at: soon"),
    )
    .unwrap_err();
    assert_eq!(e.path(), "deployed_at");

    // 128-bit integers are supported up to the NOVA integer range of this implementation.
    assert_eq!(
        to_value(&(i128::MIN, 1u128 << 100)).unwrap(),
        Value::Tuple(vec![Value::Int(i128::MIN), Value::Int(1 << 100)])
    );
    assert!(to_value(&u128::MAX).unwrap_err().to_string().contains("exceeds the NOVA integer range"));
}

#[test]
fn numeric_keys_accept_their_string_form() {
    // As after a trip through JSON, where object keys are strings.
    let m: BTreeMap<u16, String> = from_value(flow("{\"8080\": auth, \"3000\": api}")).unwrap();
    assert_eq!(m, [(8080, "auth".to_string()), (3000, "api".to_string())].into());
    let m: HashMap<bool, i8> = from_value(flow("{\"true\": 1}")).unwrap();
    assert_eq!(m[&true], 1);
    assert!(from_value::<BTreeMap<u16, String>>(flow("{x: a}")).is_err());
}

#[test]
fn collections_and_wrappers() {
    #[derive(Facet, Debug)]
    struct Borrowed<'a> {
        #[allow(dead_code)]
        s: &'a str,
    }

    #[derive(Facet, Debug, PartialEq)]
    struct C {
        bytes: Vec<u8>,
        fixed: [u8; 2],
        set: HashSet<i32>,
        boxed: Box<u32>,
        shared: Arc<String>,
        maybe: Option<Box<C>>,
        cow: Cow<'static, str>,
    }

    let c = C {
        bytes: vec![0, 255],
        fixed: [1, 2],
        set: [3].into(),
        boxed: Box::new(7),
        shared: Arc::new("s".into()),
        maybe: None,
        cow: "c".into(),
    };

    let v = to_value(&c).unwrap();
    assert_eq!(v.as_record().unwrap().get("bytes"), Some(&Value::Bytes(vec![0, 255])));
    assert_eq!(v.as_record().unwrap().get("fixed"), Some(&Value::Bytes(vec![1, 2])));
    assert_eq!(v.as_record().unwrap().get("set").unwrap().kind(), "set");
    assert_eq!(from_value::<C>(v).unwrap(), c);
    assert_eq!(from_slice::<C>(&to_vec(&c).unwrap()).unwrap(), c);
    let nested: Option<Box<Vec<Arc<u8>>>> = Some(Box::new(vec![Arc::new(1)]));
    assert_eq!(to_value(&nested).unwrap(), flow("[1]"));
    assert_eq!(from_value::<Option<Box<Vec<Arc<u8>>>>>(flow("[1]")).unwrap(), nested);
    // Byte containers also accept lists of integers.
    let c2: C = from_str("bytes: [1, 2]\nfixed: [3, 4]\nset: [1]\nboxed: 1\nshared: x\ncow: y\n").unwrap();
    assert_eq!((c2.bytes, c2.fixed), (vec![1, 2], [3, 4]));
    // A borrowed target cannot be built from an owned value.
    let e = from_value::<Borrowed<'static>>(flow("{s: x}")).unwrap_err();
    assert!(e.to_string().contains("borrowed"), "{e}");
}

#[test]
fn attributes() {
    #[derive(Facet, Debug, PartialEq)]
    #[facet(rename_all = "kebab-case", deny_unknown_fields)]
    struct Config {
        #[facet(rename = "ID")]
        id: u32,
        max_connections: u16,
        #[facet(default = 30)]
        timeout_secs: u64,
        #[facet(skip)]
        cache: Vec<u8>,
        #[facet(alias = "nick")]
        name: Option<String>,
        mode: Mode,
        port: Port,
    }

    #[derive(Facet, Debug, PartialEq)]
    #[repr(C)]
    #[facet(rename_all = "snake_case")]
    enum Mode {
        ReadOnly,
        #[facet(rename = "rw")]
        ReadWrite,
    }

    #[derive(Facet, Debug, PartialEq)]
    #[facet(transparent)]
    struct Port(u16);

    let c = Config {
        id: 1,
        max_connections: 10,
        timeout_secs: 5,
        cache: vec![1],
        name: None,
        mode: Mode::ReadWrite,
        port: Port(80),
    };

    let v = to_value(&c).unwrap();
    assert_eq!(v, flow("{ID: 1, max-connections: 10, timeout-secs: 5, name: null, mode: !rw, port: 80}"));
    let back: Config = from_str("ID: 1\nmax-connections: 10\nnick: bob\nmode: read_only\nport: 80\n").unwrap();
    assert_eq!(
        back,
        Config {
            id: 1,
            max_connections: 10,
            timeout_secs: 30,
            cache: vec![],
            name: Some("bob".into()),
            mode: Mode::ReadOnly,
            port: Port(80)
        }
    );
    let e = from_str::<Config>("ID: 1\nmax-connections: 10\nmode: rw\nport: 1\ncolour: red\n").unwrap_err();
    assert!(
        e.to_string().starts_with("unknown field `colour`, expected one of `ID`, `max-connections`"),
        "{e}"
    );
    let e = from_str::<Config>("ID: 1\nmax-connections: 10\nmode: rw\nport: x\n").unwrap_err();
    assert_eq!(e.path(), "port");
}

#[test]
fn catch_all_and_numeric_enums() {
    #[derive(Facet, Debug, PartialEq)]
    #[facet(is_numeric)]
    #[repr(u8)]
    enum Code {
        Ok = 0,
        NotFound = 4,
    }

    #[derive(Facet, Debug, PartialEq)]
    #[repr(u8)]
    enum Level {
        Low,
        High,
        #[facet(other)]
        Unknown,
    }

    assert_eq!(
        from_value::<Vec<Level>>(flow("[Low, !High, Medium]")).unwrap(),
        [Level::Low, Level::High, Level::Unknown]
    );
    assert_eq!(to_value(&[Code::Ok, Code::NotFound]).unwrap(), flow("[0, 4]"));
    assert_eq!(from_value::<Vec<Code>>(flow("[4, 0]")).unwrap(), [Code::NotFound, Code::Ok]);
}

#[test]
fn flatten() {
    #[derive(Facet, Debug, PartialEq)]
    struct Meta {
        owner: String,
        team: Option<String>,
    }

    #[derive(Facet, Debug, PartialEq)]
    struct Doc {
        id: u8,
        #[facet(flatten)]
        meta: Meta,
        #[facet(flatten)]
        rest: BTreeMap<String, i64>,
    }

    let d = Doc {
        id: 1,
        meta: Meta {
            owner: "ops".into(),
            team: Some("core".into()),
        },
        rest: [("x".to_string(), 1)].into(),
    };

    let v = to_value(&d).unwrap();
    assert_eq!(v, flow("{id: 1, owner: ops, team: core, x: 1}"));
    assert_eq!(from_value::<Doc>(v).unwrap(), d);
}

#[test]
fn internally_adjacently_tagged_and_untagged_enums() {
    #[derive(Facet, Serialize, Deserialize, Debug, PartialEq)]
    #[serde(tag = "kind")]
    #[facet(tag = "kind")]
    #[repr(u8)]
    enum Shape {
        Circle { r: f64 },
        Square { side: f64 },
    }

    #[derive(Facet, Serialize, Deserialize, Debug, PartialEq)]
    #[serde(untagged)]
    #[facet(untagged)]
    #[repr(u8)]
    enum Num {
        I(i64),
        S(String),
        P { x: i8, y: i8 },
    }

    #[derive(Facet, Serialize, Deserialize, Debug, PartialEq)]
    #[serde(tag = "t", content = "c")]
    #[facet(tag = "t", content = "c")]
    #[repr(u8)]
    enum Msg {
        Ping,
        Say(String),
    }

    let shapes = vec![Shape::Circle { r: 1.5 }, Shape::Square { side: 2.0 }];
    let t = to_string_with(&shapes, Profile::Compact).unwrap();
    assert_eq!(t, "[2]{kind,r,side}:\n  Circle,1.5,\n  Square,,2.0\n");
    assert_eq!(from_str::<Vec<Shape>>(&t).unwrap(), shapes);
    assert_eq!(to_value(&shapes).unwrap(), novafile_serde::to_value(&shapes).unwrap());

    let nums = vec![Num::I(1), Num::S("x".into()), Num::P { x: 1, y: 2 }];
    assert_eq!(from_str::<Vec<Num>>("- 1\n- x\n- {x: 1, y: 2}\n").unwrap(), nums);
    assert_eq!(to_value(&nums).unwrap(), novafile_serde::to_value(&nums).unwrap());
    let e = from_str::<Vec<Num>>("- [1]\n").unwrap_err();
    assert!(e.to_string().contains("did not match any variant of untagged enum"), "{e}");

    let msgs = vec![Msg::Ping, Msg::Say("hi".into())];
    let v = to_value(&msgs).unwrap();
    assert_eq!(v, flow("[{t: !Ping}, {t: !Say, c: hi}]"));
    assert_eq!(v, novafile_serde::to_value(&msgs).unwrap());
    assert_eq!(from_value::<Vec<Msg>>(v).unwrap(), msgs);
    // Plain strings name the tag too, as after a trip through JSON.
    assert_eq!(from_value::<Vec<Msg>>(flow("[{t: Ping}, {t: Say, c: hi}]")).unwrap(), msgs);
}

#[test]
fn proxies() {
    // A field-level proxy changes the representation of one field.
    #[derive(Facet, Debug, PartialEq)]
    struct Hex {
        #[facet(proxy = HexText)]
        n: u32,
    }

    #[derive(Facet)]
    #[facet(transparent)]
    struct HexText(String);
    impl TryFrom<HexText> for u32 {
        type Error = std::num::ParseIntError;
        fn try_from(h: HexText) -> Result<Self, Self::Error> {
            u32::from_str_radix(&h.0, 16)
        }
    }

    impl From<&u32> for HexText {
        fn from(n: &u32) -> Self {
            HexText(format!("{n:x}"))
        }
    }

    let h = Hex { n: 255 };
    assert_eq!(to_value(&h).unwrap(), flow("{n: ff}"));
    assert_eq!(from_value::<Hex>(flow("{n: ff}")).unwrap(), h);
}

#[test]
fn nova_value_types_degrade_to_text_through_their_proxy() {
    // Other facet formats see the scalar kinds through their String proxy.
    let shape = Timestamp::SHAPE;
    assert_eq!(shape.proxy.map(|p| p.shape.type_identifier), Some("String"));
}

/// Gaps against novafile-serde, kept as executable notes.
#[test]
fn known_gaps() {
    // `std::time::SystemTime` has no `Facet` impl in facet 0.46, so a field
    // of that type does not compile; `novafile_value::Timestamp` is the mapping.
    // `Vec<u8>` is bytes here, a list of integers with novafile-serde.
    let bytes = vec![1u8, 2];
    assert_eq!(to_value(&bytes).unwrap(), Value::Bytes(bytes.clone()));
    assert_eq!(novafile_serde::to_value(&bytes).unwrap(), flow("[1, 2]"));
    // facet 0.46 has no `Facet` impl for `VecDeque`.
    // Arrays are lists here, tuples with novafile-serde.
    assert_eq!(to_value(&[1, 2]).unwrap(), flow("[1, 2]"));
    assert_eq!(novafile_serde::to_value(&[1, 2]).unwrap(), flow("(1, 2)"));
}

/// A finite float beyond the f32 range is an error, not infinity.
#[test]
fn f32_overflow_is_an_error() {
    assert!(from_value::<f32>(Value::Float(1e300)).is_err());
    assert_eq!(from_value::<f32>(Value::Float(1.5)).unwrap(), 1.5);
    assert!(from_value::<f32>(Value::Float(f64::INFINITY)).unwrap().is_infinite());
}

/// A variant tag is any non-empty string (§2.11): a rename to `at start` is that tag, and
/// only a rename to the empty string is an error.
#[test]
fn renamed_variant_tags_need_not_be_identifiers() {
    #[derive(Facet)]
    #[repr(u8)]
    #[allow(dead_code)]
    enum Op {
        #[facet(rename = "at start")]
        AtStart,
        #[facet(rename = "at-end")]
        AtEnd(u8),
        #[facet(rename = "")]
        Empty,
    }

    assert_eq!(to_value(&Op::AtStart).unwrap(), Value::Variant("at start".into(), None));
    assert_eq!(to_value(&Op::AtEnd(1)).unwrap(), Value::Variant("at-end".into(), Some(Box::new(Value::Int(1)))));
    assert!(to_value(&Op::Empty).unwrap_err().to_string().contains("empty"));
}

/// `from_str_with_schema` checks the document's `@schema` line against a named schema (#28).
#[test]
fn with_schema_checks_the_schema_reference() {
    #[derive(Debug, Facet)]
    struct Settings {
        name: Option<String>,
    }

    let named = novafile_schema::parse_schema("@schema example.org/app@2.1.0\n@root Settings\nrecord Settings:\n  name?: string\n").unwrap();
    let load = |doc: &str| novafile_facet::from_str_with_schema::<Settings>(doc, &named);
    let e = load("@schema example.org/other@1.0.0\nname: x\n").unwrap_err().to_string();
    assert!(e.contains("example.org/other") && e.contains("example.org/app"), "{e}");
    assert!(
        load("@schema example.org/app@3.0.0\nname: x\n")
            .unwrap_err()
            .to_string()
            .contains("cannot read")
    );
    assert_eq!(load("@schema example.org/app@2.0.0\nname: x\n").unwrap().name.as_deref(), Some("x"));
    assert_eq!(load("name: x\n").unwrap().name.as_deref(), Some("x"));
}
