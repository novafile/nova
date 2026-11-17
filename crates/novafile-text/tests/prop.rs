//! Property tests: arbitrary values survive the binary codec and every text profile,
//! and the parsers never panic on arbitrary input.

use novafile_text::{Profile, parse, parse_document, parse_flow, to_text};
use novafile_value::{Decimal, Duration, EncodeOptions, Rational, Record, Timestamp, Value, canonical_bytes, decode, encode};
use proptest::prelude::*;

/// The longest duration in nanoseconds: `i64::MAX` seconds.
const MAX_DURATION: i128 = i64::MAX as i128 * 1_000_000_000;

/// Any scalar value, across every scalar kind.
fn scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i128>().prop_map(Value::Int),
        any::<f64>().prop_map(Value::Float),
        (any::<i128>(), -400i32..400).prop_map(|(c, e)| Value::Decimal(Decimal::new(c, e))),
        (any::<i128>(), any::<i128>()).prop_filter_map("invalid rational", |(n, d)| Rational::new(n, d).ok().map(Value::Rational)),
        ".{0,12}".prop_map(Value::String),
        prop::collection::vec(any::<u8>(), 0..12).prop_map(Value::Bytes),
        // Years 0001-9999.
        (-62_135_596_800i64..253_402_300_800, 0u32..1_000_000_000).prop_map(|(s, n)| Value::Timestamp(Timestamp::new(s, n).unwrap())),
        (-MAX_DURATION..=MAX_DURATION).prop_map(|n| Value::Duration(Duration::from_nanos(n).unwrap())),
    ]
}

/// Any value, nesting containers a few levels deep.
fn value() -> impl Strategy<Value = Value> {
    scalar().prop_recursive(4, 48, 6, |inner| {
        let items = prop::collection::vec(inner.clone(), 0..6);
        prop_oneof![
            items.clone().prop_map(Value::List),
            items.clone().prop_map(Value::Tuple),
            items.prop_filter_map("duplicate set element", |v| Value::set(v).ok()),
            prop::collection::vec((".{0,8}", inner.clone()), 0..6).prop_filter_map("duplicate record key", |e| Record::from_entries(e).ok().map(Value::Record)),
            prop::collection::vec((inner.clone(), inner.clone()), 0..4).prop_filter_map("duplicate map key", |e| Value::map(e).ok()),
            ("[a-z][a-z0-9_]{0,8}", prop::option::of(inner)).prop_map(|(n, p)| Value::Variant(n, p.map(Box::new))),
        ]
    })
}

proptest! {
    #[test]
    fn binary_round_trips(v in value(), canonical: bool, dedupe_strings: bool, tables: bool) {
        let bytes = encode(&v, EncodeOptions { canonical, dedupe_strings, tables });
        prop_assert_eq!(decode(&bytes).unwrap(), v);
    }

    #[test]
    fn canonical_bytes_are_stable(v in value()) {
        let bytes = canonical_bytes(&v);
        prop_assert_eq!(canonical_bytes(&decode(&bytes).unwrap()), bytes);
    }

    #[test]
    fn every_profile_round_trips(v in value()) {
        for p in [Profile::Human, Profile::Compact, Profile::Canonical] {
            let text = to_text(&v, p);
            let back = parse(&text).map_err(|e| TestCaseError::fail(format!("{p:?}: {e}\n{text}")))?;
            prop_assert_eq!(&back, &v, "{:?}\n{}", p, text);
        }
    }

    #[test]
    fn parsers_never_panic(src in "\\PC{0,64}") {
        let _ = parse(&src);
        let _ = parse_document(&src);
        let _ = parse_flow(&src);
    }
}
