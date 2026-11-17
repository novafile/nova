use nova_formats::toml::{from_str, to_string};
use nova_value::{Decimal, Duration, Rational, Record, Timestamp, Value};

fn rec(entries: Vec<(&str, Value)>) -> Value {
    Value::Record(Record::from_entries(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()).unwrap())
}

fn s(x: &str) -> Value {
    Value::String(x.to_owned())
}

fn ts(x: &str) -> Value {
    Value::Timestamp(Timestamp::parse(x).unwrap())
}

#[test]
fn import_kinds() {
    let v = from_str(
        r#"
title = "x"
n = -42
f = 1.5
inf = -inf
nan = nan
yes = true
list = [1, "two", [3]]
[server]
host = "a"
[[items]]
id = 1
[[items]]
id = 2
"#,
    )
    .unwrap();
    let r = v.as_record().unwrap();
    assert_eq!(r.get("title"), Some(&s("x")));
    assert_eq!(r.get("n"), Some(&Value::Int(-42)));
    assert_eq!(r.get("f"), Some(&Value::Float(1.5)));
    assert_eq!(r.get("inf"), Some(&Value::Float(f64::NEG_INFINITY)));
    assert_eq!(r.get("nan"), Some(&Value::Float(f64::NAN)));
    assert_eq!(r.get("yes"), Some(&Value::Bool(true)));
    assert_eq!(
        r.get("list"),
        Some(&Value::List(vec![Value::Int(1), s("two"), Value::List(vec![Value::Int(3)])]))
    );
    assert_eq!(r.get("server"), Some(&rec(vec![("host", s("a"))])));
    assert_eq!(
        r.get("items"),
        Some(&Value::List(vec![rec(vec![("id", Value::Int(1))]), rec(vec![("id", Value::Int(2))])]))
    );
}

#[test]
fn datetimes() {
    let v = from_str(
        "odt = 1979-05-27T07:32:00Z\noff = 1979-05-27T00:32:00.999999-07:00\nspace = 1979-05-27 07:32:00+01:30\nldt = 1979-05-27T07:32:00\nld = 1979-05-27\nlt = 07:32:00.5\n",
    )
    .unwrap();
    let r = v.as_record().unwrap();
    assert_eq!(r.get("odt"), Some(&ts("1979-05-27T07:32:00Z")));
    assert_eq!(r.get("off"), Some(&ts("1979-05-27T07:32:00.999999Z")));
    assert_eq!(r.get("space"), Some(&ts("1979-05-27T06:02:00Z")));
    // Local (zone-less) date-times, dates and times are kept as strings.
    assert_eq!(r.get("ldt"), Some(&s("1979-05-27T07:32:00")));
    assert_eq!(r.get("ld"), Some(&s("1979-05-27")));
    assert_eq!(r.get("lt"), Some(&s("07:32:00.5")));

    let v = rec(vec![("at", ts("2026-11-17T16:00:00.123456789Z")), ("old", ts("0001-01-01T00:00:00Z"))]);
    let (t, w) = to_string(&v).unwrap();
    assert!(w.is_empty());
    assert!(t.contains("at = 2026-11-17T16:00:00.123456789Z"), "{t}");
    assert_eq!(from_str(&t).unwrap(), v);
}

#[test]
fn null_and_range_errors() {
    let e = to_string(&rec(vec![("a", rec(vec![("b", Value::List(vec![Value::Int(1), Value::Null]))]))])).unwrap_err();
    assert!(e.contains(".a.b[1]") && e.contains("null"), "{e}");
    let e = to_string(&rec(vec![("big", Value::Int(i128::from(i64::MAX) + 1))])).unwrap_err();
    assert!(e.contains(".big") && e.contains("64-bit"), "{e}");
    assert!(to_string(&rec(vec![("ok", Value::Int(i64::MIN.into()))])).is_ok());
    assert!(to_string(&Value::List(vec![])).unwrap_err().contains("root"));
    assert!(to_string(&Value::Null).is_err());
}

#[test]
fn round_trip() {
    let v = rec(vec![
        ("name", s("nova \"quoted\" \\ é 🚀\nline")),
        ("n", Value::Int(i64::MAX.into())),
        ("m", Value::Int(i64::MIN.into())),
        ("f", Value::Float(0.1)),
        ("negzero", Value::Float(-0.0)),
        ("nan", Value::Float(f64::NAN)),
        ("inf", Value::Float(f64::INFINITY)),
        ("whole", Value::Float(3.0)),
        ("b", Value::Bool(false)),
        ("", s("empty key")),
        ("dotted.key", s("needs quoting")),
        (
            "mixed",
            Value::List(vec![Value::Int(1), s("x"), Value::List(vec![]), rec(vec![("a", Value::Int(1))])]),
        ),
        ("empty", Value::List(vec![])),
        ("table", rec(vec![("inner", rec(vec![("deep", Value::Bool(true))])), ("v", Value::Int(1))])),
        ("empty_table", rec(vec![])),
        (
            "products",
            Value::List(vec![
                rec(vec![("name", s("hammer")), ("dims", rec(vec![("w", Value::Int(1))]))]),
                rec(vec![]),
                rec(vec![("name", s("nail")), ("tags", Value::List(vec![rec(vec![("t", s("x"))])]))]),
            ]),
        ),
        ("at", ts("2001-02-03T04:05:06Z")),
    ]);
    let (t, w) = to_string(&v).unwrap();
    assert!(w.is_empty(), "{w:?}");
    assert!(t.contains("[[products]]"), "{t}");
    assert_eq!(from_str(&t).unwrap(), v, "{t}");
}

#[test]
fn lossy_mappings_are_reported() {
    let v = rec(vec![
        ("dec", Value::Decimal(Decimal::parse("1.5").unwrap())),
        ("rat", Value::Rational(Rational::new(22, 7).unwrap())),
        ("dur", Value::Duration(Duration::parse("90s").unwrap())),
        ("bytes", Value::Bytes(vec![1, 2, 3])),
        ("tuple", Value::Tuple(vec![Value::Int(1), s("a")])),
        ("set", Value::Set(vec![Value::Int(1)])),
        ("map", Value::map(vec![(Value::Int(1), s("one"))]).unwrap()),
        ("unit", Value::Variant("on".into(), None)),
        ("var", Value::Variant("failed".into(), Some(Box::new(Value::Tuple(vec![]))))),
    ]);
    let (t, w) = to_string(&v).unwrap();
    let paths: Vec<_> = w.iter().map(|w| w.path.as_str()).collect();
    assert_eq!(
        paths,
        [".dec", ".rat", ".dur", ".bytes", ".tuple", ".set", ".map", ".unit", ".var", ".var.failed"],
        "{w:?}"
    );
    let back = from_str(&t).unwrap();
    let r = back.as_record().unwrap();
    assert_eq!(r.get("dec"), Some(&s("1.5")));
    assert_eq!(r.get("rat"), Some(&s("22/7")));
    assert_eq!(r.get("dur"), Some(&s("1m30s")));
    assert_eq!(r.get("bytes"), Some(&s("AQID")));
    assert_eq!(r.get("map"), Some(&Value::List(vec![Value::List(vec![Value::Int(1), s("one")])])));
    assert_eq!(r.get("var"), Some(&rec(vec![("failed", Value::List(vec![]))])));
}

#[test]
fn bad_toml() {
    assert!(from_str("a = 1\na = 2\n").is_err());
    assert!(from_str("a = ").is_err());
    assert!(from_str("a = 99999999999999999999").is_err());
}
