//! Proptest strategies for arbitrary NOVA values, shared by the property tests of
//! several crates through `#[path]`.

use novafile_value::{Decimal, Duration, Rational, Record, Timestamp, Value};
use proptest::prelude::*;

/// The longest duration in nanoseconds: `i64::MAX` seconds.
pub const MAX_DURATION: i128 = i64::MAX as i128 * 1_000_000_000;

/// Earliest instant in Unix seconds: 0001-01-01T00:00:00Z.
pub const MIN_SECS: i64 = -62_135_596_800;

/// One past the latest whole second in Unix seconds: 10000-01-01T00:00:00Z.
pub const END_SECS: i64 = 253_402_300_800;

/// Any decimal, with exponents well past the range of `f64`.
pub fn decimal() -> impl Strategy<Value = Decimal> {
    (any::<i128>(), -400i32..400).prop_map(|(c, e)| Decimal::new(c, e))
}

/// Any rational number.
pub fn rational() -> impl Strategy<Value = Rational> {
    (any::<i128>(), any::<i128>()).prop_filter_map("invalid rational", |(n, d)| Rational::new(n, d).ok())
}

/// Any timestamp in the years 0001-9999.
pub fn timestamp() -> impl Strategy<Value = Timestamp> {
    (MIN_SECS..END_SECS, 0u32..1_000_000_000).prop_map(|(s, n)| Timestamp::new(s, n).expect("strategy stays in range"))
}

/// Any duration.
pub fn duration() -> impl Strategy<Value = Duration> {
    (-MAX_DURATION..=MAX_DURATION).prop_map(|n| Duration::from_nanos(n).expect("strategy stays in range"))
}

/// Any numeric value: integer, float, decimal or rational.
pub fn numeric() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i128>().prop_map(Value::Int),
        any::<f64>().prop_map(Value::Float),
        decimal().prop_map(Value::Decimal),
        rational().prop_map(Value::Rational),
    ]
}

/// Any scalar value, across every scalar kind.
pub fn scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        numeric(),
        ".{0,12}".prop_map(Value::String),
        prop::collection::vec(any::<u8>(), 0..12).prop_map(Value::Bytes),
        timestamp().prop_map(Value::Timestamp),
        duration().prop_map(Value::Duration),
    ]
}

/// Any value, nesting containers a few levels deep.
pub fn value() -> impl Strategy<Value = Value> {
    scalar().prop_recursive(4, 48, 6, |inner| {
        let items = prop::collection::vec(inner.clone(), 0..6);
        prop_oneof![
            items.clone().prop_map(Value::List),
            items.clone().prop_map(Value::Tuple),
            items.prop_filter_map("duplicate set element", |v| Value::set(v).ok()),
            prop::collection::vec((".{0,8}", inner.clone()), 0..6).prop_filter_map("duplicate record key", |e| Record::from_entries(e).ok().map(Value::Record)),
            prop::collection::vec((inner.clone(), inner.clone()), 0..4).prop_filter_map("duplicate map key", |e| Value::map(e).ok()),
            // Identifier tags, and any other non-empty string, which text writes quoted (§2.11).
            (prop_oneof!["[a-z][a-z0-9_]{0,8}", ".{1,8}"], prop::option::of(inner)).prop_map(|(n, p)| Value::Variant(n, p.map(Box::new))),
        ]
    })
}
