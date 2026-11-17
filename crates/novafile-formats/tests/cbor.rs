//! CBOR adapter tests: RFC 8949 Appendix A vectors, float widths,
//! deterministic encoding, lossy-mapping reports and decoder robustness.

use novafile_formats::cbor::{from_slice, to_vec};
use novafile_value::{Decimal, Duration, Rational, Record, Timestamp, Value};

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn rec(entries: Vec<(&str, Value)>) -> Value {
    Value::Record(Record::from_entries(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()).unwrap())
}

fn s(x: &str) -> Value {
    Value::String(x.to_owned())
}

fn i(x: i128) -> Value {
    Value::Int(x)
}

fn f(x: f64) -> Value {
    Value::Float(x)
}

fn list(v: Vec<Value>) -> Value {
    Value::List(v)
}

/// RFC 8949 Appendix A vectors whose preferred serialization is also what
/// the deterministic encoder produces: they must decode *and* encode.
#[test]
fn rfc8949_appendix_a_round_trip() {
    let one_to_25: Vec<Value> = (1..=25).map(i).collect();
    let vectors: Vec<(Value, &str)> = vec![
        (i(0), "00"),
        (i(1), "01"),
        (i(10), "0a"),
        (i(23), "17"),
        (i(24), "1818"),
        (i(25), "1819"),
        (i(100), "1864"),
        (i(1000), "1903e8"),
        (i(1_000_000), "1a000f4240"),
        (i(1_000_000_000_000), "1b000000e8d4a51000"),
        (i(18_446_744_073_709_551_615), "1bffffffffffffffff"),
        (i(18_446_744_073_709_551_616), "c249010000000000000000"),
        (i(-18_446_744_073_709_551_616), "3bffffffffffffffff"),
        (i(-18_446_744_073_709_551_617), "c349010000000000000000"),
        (i(-1), "20"),
        (i(-10), "29"),
        (i(-100), "3863"),
        (i(-1000), "3903e7"),
        (f(0.0), "f90000"),
        (f(-0.0), "f98000"),
        (f(1.0), "f93c00"),
        (f(1.1), "fb3ff199999999999a"),
        (f(1.5), "f93e00"),
        (f(65504.0), "f97bff"),
        (f(100_000.0), "fa47c35000"),
        (f(3.402_823_466_385_288_6e+38), "fa7f7fffff"),
        (f(1.0e+300), "fb7e37e43c8800759c"),
        (f(5.960_464_477_539_063e-8), "f90001"),
        (f(0.000_061_035_156_25), "f90400"),
        (f(-4.0), "f9c400"),
        (f(-4.1), "fbc010666666666666"),
        (f(f64::INFINITY), "f97c00"),
        (f(f64::NAN), "f97e00"),
        (f(f64::NEG_INFINITY), "f9fc00"),
        (Value::Bool(false), "f4"),
        (Value::Bool(true), "f5"),
        (Value::Null, "f6"),
        (
            Value::Timestamp(Timestamp::parse("2013-03-21T20:04:00Z").unwrap()),
            "c074323031332d30332d32315432303a30343a30305a",
        ),
        (Value::Bytes(vec![]), "40"),
        (Value::Bytes(vec![1, 2, 3, 4]), "4401020304"),
        (s(""), "60"),
        (s("a"), "6161"),
        (s("IETF"), "6449455446"),
        (s("\"\\"), "62225c"),
        (s("\u{fc}"), "62c3bc"),
        (s("\u{6c34}"), "63e6b0b4"),
        (s("\u{10151}"), "64f0908591"),
        (list(vec![]), "80"),
        (list(vec![i(1), i(2), i(3)]), "83010203"),
        (list(vec![i(1), list(vec![i(2), i(3)]), list(vec![i(4), i(5)])]), "8301820203820405"),
        (list(one_to_25.clone()), "98190102030405060708090a0b0c0d0e0f101112131415161718181819"),
        (rec(vec![]), "a0"),
        (Value::map(vec![(i(1), i(2)), (i(3), i(4))]).unwrap(), "a201020304"),
        (rec(vec![("a", i(1)), ("b", list(vec![i(2), i(3)]))]), "a26161016162820203"),
        (list(vec![s("a"), rec(vec![("b", s("c"))])]), "826161a161626163"),
        (
            rec(vec![("a", s("A")), ("b", s("B")), ("c", s("C")), ("d", s("D")), ("e", s("E"))]),
            "a56161614161626142616361436164614461656145",
        ),
    ];
    for (v, h) in vectors {
        let b = hex(h);
        assert_eq!(from_slice(&b).unwrap_or_else(|e| panic!("{h}: {e}")), v, "decode {h}");
        let (enc, w) = to_vec(&v, true);
        assert_eq!(w, [] as [novafile_formats::Warning; 0]);
        assert_eq!(enc, b, "encode {v:?}");
    }
}

/// Appendix A vectors that only decode (non-preferred forms, epoch tags,
/// indefinite lengths, other NaN/infinity widths).
#[test]
fn rfc8949_appendix_a_decode_only() {
    let vectors: Vec<(&str, Value)> = vec![
        ("fa7f800000", f(f64::INFINITY)),
        ("fa7fc00000", f(f64::NAN)),
        ("faff800000", f(f64::NEG_INFINITY)),
        ("fb7ff0000000000000", f(f64::INFINITY)),
        ("fb7ff8000000000000", f(f64::NAN)),
        ("fbfff0000000000000", f(f64::NEG_INFINITY)),
        ("f7", Value::Null), // undefined
        ("c11a514b67b0", Value::Timestamp(Timestamp::parse("2013-03-21T20:04:00Z").unwrap())),
        ("c1fb41d452d9ec200000", Value::Timestamp(Timestamp::parse("2013-03-21T20:04:00.5Z").unwrap())),
        ("5f42010243030405ff", Value::Bytes(vec![1, 2, 3, 4, 5])),
        ("7f657374726561646d696e67ff", s("streaming")),
        ("9fff", list(vec![])),
        ("9f018202039f0405ffff", list(vec![i(1), list(vec![i(2), i(3)]), list(vec![i(4), i(5)])])),
        ("9f01820203820405ff", list(vec![i(1), list(vec![i(2), i(3)]), list(vec![i(4), i(5)])])),
        ("83018202039f0405ff", list(vec![i(1), list(vec![i(2), i(3)]), list(vec![i(4), i(5)])])),
        ("83019f0203ff820405", list(vec![i(1), list(vec![i(2), i(3)]), list(vec![i(4), i(5)])])),
        ("9f0102030405060708090a0b0c0d0e0f101112131415161718181819ff", list((1..=25).map(i).collect())),
        ("bf61610161629f0203ffff", rec(vec![("a", i(1)), ("b", list(vec![i(2), i(3)]))])),
        ("826161bf61626163ff", list(vec![s("a"), rec(vec![("b", s("c"))])])),
        ("bf6346756ef563416d7421ff", rec(vec![("Fun", Value::Bool(true)), ("Amt", i(-2))])),
        // Non-shortest argument encodings are accepted.
        ("1b0000000000000001", i(1)),
        ("7800", s("")),
    ];
    for (h, v) in vectors {
        assert_eq!(from_slice(&hex(h)).unwrap_or_else(|e| panic!("{h}: {e}")), v, "{h}");
    }
}

#[test]
fn unsupported_items_are_errors() {
    for (h, why) in [
        ("f0", "simple value"),              // simple(16)
        ("f8ff", "simple value"),            // simple(255)
        ("d74401020304", "unsupported tag"), // 23(h'01020304')
        ("d818456449455446", "unsupported tag"),
        ("d82076687474703a2f2f7777772e6578616d706c652e636f6d", "unsupported tag"),
        ("1c", "reserved"),
        ("ff", "break"),
        ("c06161", "tag 0"),
    ] {
        let e = from_slice(&hex(h)).unwrap_err();
        assert!(e.contains(why), "{h}: {e}");
    }
}

#[test]
fn f16_decoding() {
    for (h, want) in [
        ("f90000", 0.0),
        ("f93c00", 1.0),
        ("f93e00", 1.5),
        ("f97bff", 65504.0),
        ("f90001", 5.960_464_477_539_063e-8),
        ("f903ff", 0.000_060_975_551_605_224_61),
        ("f90400", 0.000_061_035_156_25),
        ("f9c400", -4.0),
        ("f9bc00", -1.0),
        ("f93555", 0.333_251_953_125),
    ] {
        assert_eq!(from_slice(&hex(h)).unwrap(), f(want), "{h}");
    }

    let Value::Float(z) = from_slice(&hex("f98000")).unwrap() else { panic!() };
    assert!(z == 0.0 && z.is_sign_negative());
    assert_eq!(from_slice(&hex("f97c01")).unwrap(), f(f64::NAN));
}

#[test]
fn float_widths() {
    // Non-deterministic mode always writes binary64.
    assert_eq!(to_vec(&f(1.5), false).0, hex("fb3ff8000000000000"));
    assert_eq!(to_vec(&f(f64::NAN), false).0, hex("fb7ff8000000000000"));
    // Deterministic: shortest exact width.
    assert_eq!(to_vec(&f(0.5), true).0, hex("f93800"));
    assert_eq!(to_vec(&f(f64::from(0.1f32)), true).0, hex("fa3dcccccd"));
    assert_eq!(to_vec(&f(0.1), true).0, hex("fb3fb999999999999a"));
    assert_eq!(to_vec(&f(65520.0), true).0, hex("fa477ff000"));
    assert_eq!(to_vec(&f(2.980_232_238_769_531_2e-8), true).0, hex("fa33000000")); // 2^-25: below f16
    assert_eq!(to_vec(&f(f64::from_bits(1)), true).0, hex("fb0000000000000001"));
}

#[test]
fn deterministic_map_ordering() {
    // RFC 8949 §4.2.1: keys sorted by the bytewise order of their encodings,
    // so shorter encodings sort first (10 < 100 < -1 < "z" < "aa" < [100] < [-1] < false).
    let v = Value::map(vec![
        (Value::Bool(false), i(0)),
        (list(vec![i(-1)]), i(0)),
        (list(vec![i(100)]), i(0)),
        (s("aa"), i(0)),
        (s("z"), i(0)),
        (i(-1), i(0)),
        (i(100), i(0)),
        (i(10), i(0)),
    ])
    .unwrap();
    let (b, _) = to_vec(&v, true);
    assert_eq!(b, hex("a8 0a00 186400 2000 617a00 62616100 81186400 812000 f400"));
    // Records too, and the result does not depend on insertion order.
    let a = rec(vec![("b", i(1)), ("aa", i(2)), ("a", i(3))]);
    let c = rec(vec![("a", i(3)), ("aa", i(2)), ("b", i(1))]);
    assert_eq!(to_vec(&a, true).0, hex("a3 616103 616201 62616102"));
    assert_eq!(to_vec(&a, true).0, to_vec(&c, true).0);
    // Sets are sorted as well, so equal sets encode identically.
    let s1 = Value::set(vec![i(3), i(1), s("x")]).unwrap();
    let s2 = Value::set(vec![s("x"), i(3), i(1)]).unwrap();
    assert_eq!(to_vec(&s1, true).0, hex("d90102 83 01 03 6178"));
    assert_eq!(to_vec(&s1, true).0, to_vec(&s2, true).0);
    // Non-deterministic mode keeps insertion order.
    assert_eq!(to_vec(&a, false).0, hex("a3 616201 62616102 616103"));
}

#[test]
fn round_trip_every_kind() {
    let v = rec(vec![
        ("null", Value::Null),
        ("bools", list(vec![Value::Bool(true), Value::Bool(false)])),
        (
            "ints",
            list(vec![
                i(0),
                i(-1),
                i(23),
                i(24),
                i(u64::MAX.into()),
                i(i128::from(u64::MAX) + 1),
                i(-i128::from(u64::MAX) - 2),
                i(i128::MAX),
                i(i128::MIN),
            ]),
        ),
        (
            "floats",
            list(vec![
                f(0.0),
                f(-0.0),
                f(f64::NAN),
                f(f64::INFINITY),
                f(f64::NEG_INFINITY),
                f(0.1),
                f(1e300),
                f(f64::MIN_POSITIVE),
                f(f64::from_bits(1)),
            ]),
        ),
        (
            "decimals",
            list(vec![
                Value::Decimal(Decimal::parse("1.5").unwrap()),
                Value::Decimal(Decimal::parse("-0.000123").unwrap()),
                Value::Decimal(Decimal::parse("0").unwrap()),
                Value::Decimal(Decimal::parse("12345678901234567890123456789012345678e-20").unwrap()),
                Value::Decimal(Decimal::new(7, i32::MAX)),
                Value::Decimal(Decimal::new(-7, i32::MIN)),
            ]),
        ),
        (
            "rationals",
            list(vec![
                Value::Rational(Rational::new(22, 7).unwrap()),
                Value::Rational(Rational::new(-1, 3).unwrap()),
                Value::Rational(Rational::new(0, 5).unwrap()),
                Value::Rational(Rational::new(i128::MAX, i128::MAX - 1).unwrap()),
            ]),
        ),
        ("unicode", s("héllo 水 🚀 \u{0} \u{feff}")),
        ("bytes", Value::Bytes((0..=255).collect())),
        (
            "times",
            list(vec![
                Value::Timestamp(Timestamp::parse("2026-11-17T16:00:00.123456789Z").unwrap()),
                Value::Timestamp(Timestamp::parse("0001-01-01T00:00:00Z").unwrap()),
                Value::Timestamp(Timestamp::parse("9999-12-31T23:59:59.999999999Z").unwrap()),
            ]),
        ),
        ("set", Value::set(vec![i(1), s("1"), list(vec![]), Value::set(vec![]).unwrap()]).unwrap()),
        ("empty_set", Value::set(vec![]).unwrap()),
        (
            "map",
            Value::map(vec![(i(1), s("one")), (list(vec![i(1)]), Value::Null), (rec(vec![("k", i(1))]), i(2))]).unwrap(),
        ),
        ("string_map", Value::map(vec![(s("a"), i(1)), (s("b"), i(2))]).unwrap()),
        ("empty_map", Value::map(vec![]).unwrap()),
        ("empty_record", rec(vec![])),
        ("nested", list(vec![list(vec![list(vec![rec(vec![("deep", list(vec![i(1)]))])])])])),
    ]);
    for det in [false, true] {
        let (b, w) = to_vec(&v, det);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(from_slice(&b).unwrap(), v, "deterministic={det}");
    }
}

#[test]
fn lossy_mappings_are_reported() {
    let v = rec(vec![
        ("dur", Value::Duration(Duration::parse("1h30m").unwrap())),
        ("tuple", Value::Tuple(vec![i(1), s("a")])),
        ("unit", Value::Variant("active".into(), None)),
        ("payload", Value::Variant("failed".into(), Some(Box::new(Value::Tuple(vec![]))))),
    ]);
    let (b, w) = to_vec(&v, true);
    let paths: Vec<_> = w.iter().map(|w| w.path.as_str()).collect();
    assert_eq!(paths, [".dur", ".tuple", ".unit", ".payload", ".payload.failed"], "{w:?}");
    let back = from_slice(&b).unwrap();
    let r = back.as_record().unwrap();
    assert_eq!(r.get("dur"), Some(&s("1h30m")));
    assert_eq!(r.get("tuple"), Some(&list(vec![i(1), s("a")])));
    assert_eq!(r.get("unit"), Some(&s("active")));
    assert_eq!(r.get("payload"), Some(&rec(vec![("failed", list(vec![]))])));
}

#[test]
fn tagged_number_and_set_validation() {
    // 4([-2, 27315]) = 273.15 (RFC 8949 §3.4.4).
    assert_eq!(from_slice(&hex("c48221196ab3")).unwrap(), Value::Decimal(Decimal::parse("273.15").unwrap()));
    // Decimal fraction with a bignum mantissa.
    let d = from_slice(&hex("c48220c249010000000000000000")).unwrap();
    assert_eq!(d, Value::Decimal(Decimal::parse("1844674407370955161.6").unwrap()));
    // 30([2, 4]) normalizes to 1/2.
    assert_eq!(from_slice(&hex("d81e820204")).unwrap(), Value::Rational(Rational::new(1, 2).unwrap()));
    assert!(from_slice(&hex("d81e820100")).is_err()); // zero denominator
    assert!(from_slice(&hex("d81e820120")).is_err()); // negative denominator
    assert!(from_slice(&hex("c483010203")).is_err()); // three elements
    assert!(from_slice(&hex("c482616101")).is_err()); // text exponent
    assert!(from_slice(&hex("c4821a800000000a")).unwrap_err().contains("32-bit")); // exponent 2^31
    assert!(from_slice(&hex("c4821a7fffffff0a")).unwrap_err().contains("32-bit")); // 10 × 10^(2^31-1)
    assert_eq!(from_slice(&hex("c4821a7fffffff07")).unwrap(), Value::Decimal(Decimal::new(7, i32::MAX)));
    // Bignums beyond i128.
    assert!(from_slice(&hex("c2510100000000000000000000000000000000")).is_err());
    assert!(from_slice(&hex("c25080000000000000000000000000000000")).unwrap_err().contains("range"));
    assert_eq!(from_slice(&hex("c3507fffffffffffffffffffffffffffffff")).unwrap(), i(i128::MIN));
    // Leading zero bytes in a bignum are tolerated.
    assert_eq!(from_slice(&hex("c243000001")).unwrap(), i(1));
    // Duplicate set elements and map keys.
    assert!(from_slice(&hex("d9010282 01 01")).unwrap_err().contains("duplicate"));
    assert!(from_slice(&hex("a2 6161 01 6161 02")).unwrap_err().contains("duplicate"));
    assert!(from_slice(&hex("a2 01 01 01 02")).unwrap_err().contains("duplicate"));
    assert!(from_slice(&hex("bf 01 01 01 02 ff")).unwrap_err().contains("duplicate"));
    // Tag 259 keeps a text-keyed map a map.
    assert_eq!(from_slice(&hex("d90103 a1 6161 01")).unwrap(), Value::map(vec![(s("a"), i(1))]).unwrap());
    assert!(from_slice(&hex("d90103 80")).is_err());
}

#[test]
fn truncated_and_malformed_input() {
    let good = to_vec(
        &rec(vec![
            ("a", list(vec![i(1_000_000), s("hello"), f(0.1), Value::Bytes(vec![1, 2, 3])])),
            ("b", i(i128::MAX)),
        ]),
        true,
    )
    .0;
    for n in 0..good.len() {
        assert!(from_slice(&good[..n]).is_err(), "prefix {n}");
    }

    assert!(from_slice(&good).is_ok());
    for h in [
        "",
        "18",
        "1901",
        "5f",                   // unterminated indefinite bytes
        "5f4101",               // unterminated after a chunk
        "5f6161ff",             // text chunk inside bytes
        "7f5f41ffff",           // nested indefinite chunk
        "9f01",                 // unterminated indefinite array
        "bf6161",               // map key without value
        "a16161",               // definite map key without value
        "62c328",               // invalid UTF-8
        "7f61c361bcff",         // UTF-8 split across chunks
        "0001",                 // trailing bytes
        "1f",                   // indefinite integer
        "5b00000000ffffffff",   // byte string longer than input
        "9b00000000ffffffff",   // array longer than input
        "bb7fffffffffffffff",   // map longer than input
        "c1fb7ff0000000000000", // epoch infinity
        "c11b7fffffffffffffff", // epoch out of range
    ] {
        assert!(from_slice(&hex(h)).is_err(), "{h}");
    }
}

#[test]
fn depth_limit() {
    let mut deep = vec![0x81; 200];
    deep.push(0x80);
    assert!(from_slice(&deep).unwrap_err().contains("nesting"));
    let mut tags = vec![0xc2; 200];
    tags.push(0x40);
    assert!(from_slice(&tags).unwrap_err().contains("nesting"));
    let mut ok = vec![0x81; 100];
    ok.push(0x80);
    assert!(from_slice(&ok).is_ok());
}
