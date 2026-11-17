use nova_formats::yaml::{from_str, from_str_multi, to_string};
use nova_value::{Decimal, Duration, Rational, Record, Timestamp, Value};

fn rec(entries: Vec<(&str, Value)>) -> Value {
    Value::Record(Record::from_entries(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()).unwrap())
}

fn s(x: &str) -> Value {
    Value::String(x.to_owned())
}

#[test]
fn core_schema_resolution_table() {
    let cases: Vec<(&str, Value)> = vec![
        ("", Value::Null),
        ("~", Value::Null),
        ("null", Value::Null),
        ("Null", Value::Null),
        ("NULL", Value::Null),
        ("nULL", s("nULL")),
        ("true", Value::Bool(true)),
        ("True", Value::Bool(true)),
        ("TRUE", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("FALSE", Value::Bool(false)),
        ("tRUE", s("tRUE")),
        ("0", Value::Int(0)),
        ("-0", Value::Int(0)),
        ("+12", Value::Int(12)),
        ("-17", Value::Int(-17)),
        ("0o17", Value::Int(15)),
        ("0x1F", Value::Int(31)),
        ("0xff", Value::Int(255)),
        ("170141183460469231731687303715884105727", Value::Int(i128::MAX)),
        ("-170141183460469231731687303715884105728", Value::Int(i128::MIN)),
        ("1.5", Value::Float(1.5)),
        ("-1.5", Value::Float(-1.5)),
        ("1.", Value::Float(1.0)),
        (".5", Value::Float(0.5)),
        ("1e3", Value::Float(1000.0)),
        ("1.5E-3", Value::Float(0.0015)),
        ("-0.0", Value::Float(-0.0)),
        (".inf", Value::Float(f64::INFINITY)),
        ("+.Inf", Value::Float(f64::INFINITY)),
        ("-.INF", Value::Float(f64::NEG_INFINITY)),
        (".nan", Value::Float(f64::NAN)),
        (".NaN", Value::Float(f64::NAN)),
        (".NAN", Value::Float(f64::NAN)),
        // YAML 1.1 forms stay strings.
        ("yes", s("yes")),
        ("No", s("No")),
        ("on", s("on")),
        ("OFF", s("OFF")),
        ("y", s("y")),
        ("0777", s("0777")),
        ("00.5", s("00.5")),
        ("0b1010", s("0b1010")),
        ("1_000", s("1_000")),
        ("1:20", s("1:20")),
        ("2001-12-14", s("2001-12-14")),
        ("0o8", s("0o8")),
        ("0x", s("0x")),
        ("-0x1", s("-0x1")),
        (".", s(".")),
        ("1e", s("1e")),
        ("inf", s("inf")),
        ("nan", s("nan")),
        ("hello world", s("hello world")),
    ];
    for (src, want) in cases {
        let got = from_str(&format!("v: {src}\n")).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        assert_eq!(got.as_record().unwrap().get("v"), Some(&want), "{src:?}");
    }
}

#[test]
fn quoted_and_block_scalars_are_strings() {
    let v = from_str("a: '1'\nb: \"true\"\nc: |\n  null\nd: >\n  12\n").unwrap();
    assert_eq!(v, rec(vec![("a", s("1")), ("b", s("true")), ("c", s("null\n")), ("d", s("12\n"))]));
}

#[test]
fn big_and_overflowing_ints() {
    assert!(from_str("170141183460469231731687303715884105728").unwrap_err().contains("range"));
    assert!(from_str("0x1ffffffffffffffffffffffffffffffff").unwrap_err().contains("range"));
    assert!(from_str("1e400").unwrap_err().contains("overflow"));
}

#[test]
fn core_tags() {
    let v = from_str("a: !!str 12\nb: !!int '12'\nc: !!float 1\nd: !!bool \"true\"\ne: !!null ''\nf: !!seq [1]\ng: !!map {x: 1}\nh: ! 12\ni: !!binary AAEC\n")
        .unwrap();
    assert_eq!(
        v,
        rec(vec![
            ("a", s("12")),
            ("b", Value::Int(12)),
            ("c", Value::Float(1.0)),
            ("d", Value::Bool(true)),
            ("e", Value::Null),
            ("f", Value::List(vec![Value::Int(1)])),
            ("g", rec(vec![("x", Value::Int(1))])),
            ("h", s("12")),
            ("i", Value::Bytes(vec![0, 1, 2])),
        ])
    );
    for bad in [
        "!!int abc",
        "!!bool yes",
        "!!null x",
        "!!timestamp 2001-12-14",
        "!custom x",
        "!!set {a}",
        "!!map [1]",
        "!!seq x",
    ] {
        assert!(from_str(bad).is_err(), "{bad}");
    }
    assert!(from_str("!!python/object:os.system x").unwrap_err().contains("unsupported tag"));
}

#[test]
fn mappings_records_and_maps() {
    let v = from_str("1: a\n[x]: b\n").unwrap();
    assert_eq!(v, Value::map(vec![(Value::Int(1), s("a")), (Value::List(vec![s("x")]), s("b"))]).unwrap());
    assert!(from_str("a: 1\na: 2\n").unwrap_err().contains("duplicate"));
    assert!(from_str("1: 1\n1: 2\n").unwrap_err().contains("duplicate"));
    // `"1"` (string) and `1` (int) are different keys.
    assert!(from_str("'1': 1\n1: 2\n").is_ok());
}

#[test]
fn aliases_expand() {
    let v = from_str("base: &b {x: 1}\nother: *b\nlist: [&n 5, *n]\n").unwrap();
    let r = v.as_record().unwrap();
    assert_eq!(r.get("base"), r.get("other"));
    assert_eq!(r.get("list"), Some(&Value::List(vec![Value::Int(5), Value::Int(5)])));
}

#[test]
fn alias_bomb_is_rejected_quickly() {
    let mut doc = String::from("a0: &a0 [lol, lol, lol, lol, lol, lol, lol, lol, lol, lol]\n");
    for i in 1..10 {
        let p = i - 1;
        doc.push_str(&format!("a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n"));
    }
    let start = std::time::Instant::now();
    let e = from_str(&doc).unwrap_err();
    assert!(e.contains("budget"), "{e}");
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    // A document comfortably within the budget still loads.
    assert!(from_str("a: &a [1, 2, 3]\nb: [*a, *a, *a]\n").is_ok());
}

#[test]
fn recursive_and_unknown_aliases_fail() {
    assert!(from_str("a: &a [*a]\n").is_err());
    assert!(from_str("a: *nope\n").is_err());
}

#[test]
fn merge_keys_are_rejected() {
    let e = from_str("base: &b {x: 1}\nderived:\n  <<: *b\n  y: 2\n").unwrap_err();
    assert!(e.contains("merge keys") && e.contains("spread"), "{e}");
    // A quoted `<<` is an ordinary key.
    assert!(from_str("'<<': 1\n").is_ok());
}

#[test]
fn depth_limit() {
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    assert!(from_str(&deep).unwrap_err().contains("nesting"));
    let ok = format!("{}{}", "[".repeat(100), "]".repeat(100));
    assert!(from_str(&ok).is_ok());
}

#[test]
fn documents() {
    assert_eq!(from_str("").unwrap(), Value::Null);
    assert!(from_str("a\n---\nb\n").unwrap_err().contains("more than one document"));
    assert_eq!(from_str_multi("a\n---\nb\n...\n--- 3\n").unwrap(), vec![s("a"), s("b"), Value::Int(3)]);
    assert_eq!(from_str_multi("").unwrap(), vec![]);
}

#[test]
fn ambiguous_strings_are_quoted() {
    let tricky = [
        "",
        "null",
        "Null",
        "~",
        "true",
        "False",
        "yes",
        "no",
        "On",
        "off",
        "y",
        "N",
        "12",
        "-3",
        "0777",
        "0o7",
        "0x1f",
        "1.5",
        ".5",
        "1e3",
        ".inf",
        "-.inf",
        ".nan",
        "2001-12-14",
        "1:20",
        "0b11",
        "<<",
        "- a",
        "a: b",
        "a #b",
        "#c",
        "&x",
        "*x",
        "!t",
        "|",
        ">",
        "@x",
        "`x",
        "%x",
        "'q'",
        "\"q\"",
        "[x]",
        "{x}",
        "a,b",
        "?",
        "-",
        "---",
        "...",
        " lead",
        "trail ",
        "multi\nline",
        "tab\there",
        "nul\0",
        "bell\u{7}",
        "nel\u{85}",
        "ls\u{2028}",
        "bom\u{feff}",
        "back\\slash",
        "é",
        "🚀",
        "=",
    ];
    for t in tricky {
        let v = rec(vec![("k", s(t)), (t, Value::Int(1))]);
        let (y, w) = to_string(&v);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(from_str(&y).unwrap_or_else(|e| panic!("{t:?}: {e}\n{y}")), v, "{t:?}\n{y}");
        let (y, _) = to_string(&s(t));
        assert_eq!(from_str(&y).unwrap(), s(t), "{t:?}\n{y}");
    }
    // Ambiguous strings are actually quoted, ordinary ones are not.
    assert_eq!(to_string(&s("yes")).0, "\"yes\"\n");
    assert_eq!(to_string(&s("0777")).0, "\"0777\"\n");
    assert_eq!(to_string(&s("hello world")).0, "hello world\n");
}

fn json_subset() -> Value {
    rec(vec![
        ("name", s("nova")),
        ("n", Value::Int(42)),
        ("big", Value::Int(i128::MAX)),
        ("neg", Value::Int(i128::MIN)),
        ("f", Value::Float(1.5)),
        ("tiny", Value::Float(1e-300)),
        ("huge", Value::Float(1.7976931348623157e308)),
        ("third", Value::Float(1.0 / 3.0)),
        ("negzero", Value::Float(-0.0)),
        ("nan", Value::Float(f64::NAN)),
        ("inf", Value::Float(f64::NEG_INFINITY)),
        ("whole", Value::Float(2.0)),
        ("t", Value::Bool(true)),
        ("nothing", Value::Null),
        ("unicode", s("héllo wörld 水 🚀")),
        ("empty_list", Value::List(vec![])),
        ("empty_rec", rec(vec![])),
        (
            "nested",
            Value::List(vec![
                Value::List(vec![Value::Int(1), Value::List(vec![Value::Int(2)]), Value::List(vec![])]),
                rec(vec![("a", rec(vec![("b", Value::List(vec![rec(vec![("c", Value::Null)])]))])), ("d", s("x"))]),
                Value::Null,
                s(""),
            ]),
        ),
        ("multi", s("line one\nline two\n")),
    ])
}

#[test]
fn round_trip_json_subset() {
    let v = json_subset();
    let (y, w) = to_string(&v);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(from_str(&y).unwrap(), v, "{y}");
    for top in [
        Value::Null,
        Value::Int(-5),
        Value::Float(-0.0),
        s("x"),
        Value::List(vec![Value::Int(1)]),
        rec(vec![]),
    ] {
        let (y, _) = to_string(&top);
        assert_eq!(from_str(&y).unwrap(), top, "{y}");
    }
}

#[test]
fn block_layout() {
    let v = rec(vec![
        (
            "a",
            Value::List(vec![
                Value::Int(1),
                rec(vec![("b", Value::Int(2)), ("c", Value::Int(3))]),
                Value::List(vec![Value::Int(4), Value::Int(5)]),
            ]),
        ),
        ("d", rec(vec![("e", s("f"))])),
    ]);
    assert_eq!(to_string(&v).0, "a:\n  - 1\n  - b: 2\n    c: 3\n  - - 4\n    - 5\nd:\n  e: f\n");
}

#[test]
fn maps_with_non_string_keys() {
    let v = Value::map(vec![
        (Value::Int(1), s("one")),
        (Value::Bool(false), Value::List(vec![Value::Int(0)])),
        (Value::List(vec![Value::Int(1), Value::Int(2)]), rec(vec![("x", Value::Int(1))])),
        (rec(vec![("k", s("v"))]), Value::Null),
        (Value::Null, s("null key")),
    ])
    .unwrap();
    let (y, w) = to_string(&v);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(from_str(&y).unwrap(), v, "{y}");
}

#[test]
fn lossy_mappings_are_reported() {
    let v = rec(vec![
        ("dec", Value::Decimal(Decimal::parse("1.50").unwrap())),
        ("rat", Value::Rational(Rational::new(1, 3).unwrap())),
        ("ts", Value::Timestamp(Timestamp::parse("2026-11-17T16:00:00.5Z").unwrap())),
        ("dur", Value::Duration(Duration::parse("1h30m").unwrap())),
        ("bytes", Value::Bytes(vec![0, 1, 2, 255])),
        ("tuple", Value::Tuple(vec![Value::Int(1)])),
        ("set", Value::Set(vec![Value::Int(1)])),
        ("smap", Value::map(vec![(s("a"), Value::Int(1))]).unwrap()),
        ("unit", Value::Variant("active".into(), None)),
        ("payload", Value::Variant("failed".into(), Some(Box::new(s("disk full"))))),
    ]);
    let (y, w) = to_string(&v);
    let paths: Vec<_> = w.iter().map(|w| w.path.as_str()).collect();
    assert_eq!(
        paths,
        [".dec", ".rat", ".ts", ".dur", ".bytes", ".tuple", ".set", ".smap", ".unit", ".payload"],
        "{w:?}"
    );
    assert_eq!(w[0].to_string(), ".dec: decimal written as a string");
    let back = from_str(&y).unwrap();
    let r = back.as_record().unwrap();
    assert_eq!(r.get("dec"), Some(&s("1.5")));
    assert_eq!(r.get("rat"), Some(&s("1/3")));
    assert_eq!(r.get("ts"), Some(&s("2026-11-17T16:00:00.5Z")));
    assert_eq!(r.get("bytes"), Some(&Value::Bytes(vec![0, 1, 2, 255])));
    assert_eq!(r.get("tuple"), Some(&Value::List(vec![Value::Int(1)])));
    assert_eq!(r.get("smap"), Some(&rec(vec![("a", Value::Int(1))])));
    assert_eq!(r.get("unit"), Some(&s("active")));
    assert_eq!(r.get("payload"), Some(&rec(vec![("failed", s("disk full"))])));
}

#[test]
fn long_keys_use_explicit_entries() {
    let k = "k".repeat(2000);
    let v = rec(vec![(&k, Value::Int(1))]);
    let (y, _) = to_string(&v);
    assert!(y.starts_with("? "));
    assert_eq!(from_str(&y).unwrap(), v);
}
