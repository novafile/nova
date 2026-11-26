//! Property tests: the binary codec round-trips and never panics, and the scalar
//! types keep their invariants through parsing, display and comparison.

#[path = "support/strategy.rs"]
mod strategy;

use std::cmp::Ordering;

use novafile_value::{Decimal, Duration, EncodeOptions, Rational, Record, Timestamp, Value, canonical_bytes, decode, decode_ref, encode, numeric_cmp};
use proptest::prelude::*;
use strategy::{decimal, duration, numeric, rational, timestamp, value};

proptest! {
    /// Every encoder mode gives bytes that decode to the same value.
    #[test]
    fn binary_round_trips(v in value(), canonical: bool, dedupe_strings: bool, tables: bool) {
        let bytes = encode(&v, EncodeOptions { canonical, dedupe_strings, tables });
        prop_assert_eq!(decode(&bytes).unwrap(), v);
    }

    /// The canonical encoding is a fixed point of decode and re-encode.
    #[test]
    fn canonical_bytes_are_stable(v in value()) {
        let bytes = canonical_bytes(&v);
        prop_assert_eq!(canonical_bytes(&decode(&bytes).unwrap()), bytes);
    }

    /// Equal values have equal canonical bytes, whatever encoding they came from.
    #[test]
    fn canonical_bytes_ignore_encoder_mode(v in value(), dedupe_strings: bool, tables: bool) {
        let bytes = encode(&v, EncodeOptions { canonical: false, dedupe_strings, tables });
        prop_assert_eq!(canonical_bytes(&decode(&bytes).unwrap()), canonical_bytes(&v));
    }

    /// The decoders never panic on arbitrary bytes, and the owned and borrowed decoders agree.
    #[test]
    fn decoders_agree_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        match (decode(&bytes), decode_ref(&bytes)) {
            (Ok(v), Ok(r)) => prop_assert_eq!(r.to_value(), v),
            (Err(e), Err(r)) => prop_assert_eq!(e, r),
            (a, b) => prop_assert!(false, "owned {:?} but borrowed {:?}", a, b.map(|r| r.to_value())),
        }
    }

    /// Flipping one byte of a valid encoding never panics the decoder.
    #[test]
    fn decoders_survive_corrupted_encodings(v in value(), at: prop::sample::Index, bits in 1u8..) {
        let mut bytes = canonical_bytes(&v);
        let i = at.index(bytes.len());
        bytes[i] ^= bits;
        let _ = decode(&bytes);
        let _ = decode_ref(&bytes);
    }

    /// A decimal has no trailing zero in its coefficient, and zero has exponent 0.
    #[test]
    fn decimals_are_normalized(d in decimal()) {
        if d.coefficient() == 0 {
            prop_assert_eq!(d.exponent(), 0);
        } else {
            prop_assert_ne!(d.coefficient() % 10, 0);
        }
    }

    /// Scaling the coefficient by ten and lowering the exponent gives the same decimal.
    #[test]
    fn decimal_trailing_zeros_do_not_matter(c in -(i128::MAX / 10)..=i128::MAX / 10, e in -400i32..400) {
        prop_assert_eq!(Decimal::new(c * 10, e), Decimal::new(c, e + 1));
    }

    /// A decimal's display form parses back to the same decimal.
    #[test]
    fn decimal_display_parses_back(d in decimal()) {
        prop_assert_eq!(Decimal::parse(&d.to_string()), Ok(d));
    }

    /// A rational has a positive denominator coprime to its numerator.
    #[test]
    fn rationals_are_normalized(r in rational()) {
        prop_assert!(r.denominator() > 0);
        let (mut a, mut b) = (r.numerator().unsigned_abs(), r.denominator().unsigned_abs());
        while b != 0 {
            (a, b) = (b, a % b);
        }
        prop_assert_eq!(a, 1);
    }

    /// Scaling both components by the same factor gives the same rational.
    #[test]
    fn rational_scaling_does_not_matter(n in -1_000_000_000i128..1_000_000_000, d in 1i128..1_000_000_000, k in -1_000_000_000i128..1_000_000_000) {
        prop_assume!(k != 0);
        prop_assert_eq!(Rational::new(n * k, d * k), Rational::new(n, d));
    }

    /// A rational's display form parses back to the same rational.
    #[test]
    fn rational_display_parses_back(r in rational()) {
        prop_assert_eq!(Rational::parse(&r.to_string()), Ok(r));
    }

    /// A timestamp's display form parses back to the same instant.
    #[test]
    fn timestamp_display_parses_back(t in timestamp()) {
        prop_assert_eq!(Timestamp::parse(&t.to_string()), Ok(t));
    }

    /// A duration's NOVA and ISO 8601 forms both parse back to the same duration.
    #[test]
    fn duration_forms_parse_back(d in duration()) {
        prop_assert_eq!(Duration::parse(&d.to_string()), Ok(d));
        prop_assert_eq!(Duration::parse_iso8601(&d.to_iso8601()), Ok(d));
    }

    /// The scalar parsers never panic on arbitrary text.
    #[test]
    fn scalar_parsers_never_panic(s in "\\PC{0,40}") {
        let _ = Decimal::parse(&s);
        let _ = Rational::parse(&s);
        let _ = Timestamp::parse(&s);
        let _ = Duration::parse(&s);
        let _ = Duration::parse_iso8601(&s);
    }

    /// Numeric comparison is reflexive and antisymmetric across numeric kinds.
    #[test]
    fn numeric_cmp_is_antisymmetric(a in numeric(), b in numeric()) {
        if let Some(o) = numeric_cmp(&a, &a) {
            prop_assert_eq!(o, Ordering::Equal);
        }
        prop_assert_eq!(numeric_cmp(&a, &b), numeric_cmp(&b, &a).map(Ordering::reverse));
    }

    /// Numeric comparison is transitive across numeric kinds.
    #[test]
    fn numeric_cmp_is_transitive(a in numeric(), b in numeric(), c in numeric()) {
        if let (Some(ab), Some(bc)) = (numeric_cmp(&a, &b), numeric_cmp(&b, &c))
            && ab == bc
        {
            prop_assert_eq!(numeric_cmp(&a, &c), Some(ab));
        }
    }

    /// An integer, the equal decimal and the equal rational compare equal, and
    /// integers compare like `i128`.
    #[test]
    fn numeric_cmp_agrees_with_integers(i: i128, j: i128) {
        prop_assume!(i != i128::MIN);
        let as_dec = Value::Decimal(Decimal::from_int(i));
        let as_rat = Value::Rational(Rational::new(i, 1).unwrap());
        prop_assert_eq!(numeric_cmp(&Value::Int(i), &as_dec), Some(Ordering::Equal));
        prop_assert_eq!(numeric_cmp(&Value::Int(i), &as_rat), Some(Ordering::Equal));
        prop_assert_eq!(numeric_cmp(&Value::Int(i), &Value::Int(j)), Some(i.cmp(&j)));
        prop_assert_eq!(numeric_cmp(&as_dec, &Value::Int(j)), Some(i.cmp(&j)));
    }
}

/// A configuration-like layer: records and maps over a few keys, so that two layers share
/// keys, and scalars and lists among them, so that kinds clash at a shared key.
fn layer() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        (0i64..3).prop_map(Value::from),
        "[xy]".prop_map(Value::String),
        prop::collection::vec((0i64..3).prop_map(Value::from), 0..3).prop_map(Value::List),
    ];
    leaf.prop_recursive(3, 32, 4, |inner| {
        prop_oneof![
            prop::collection::btree_map("[a-c]", inner.clone(), 0..4).prop_map(|m| Value::Record(Record::from_entries(m.into_iter().collect()).unwrap())),
            prop::collection::btree_map(0i64..3, inner, 0..3).prop_map(|m| Value::Map(m.into_iter().map(|(k, v)| (Value::from(k), v)).collect())),
        ]
    })
}

/// One step of a path through records (by field) and maps (by key).
#[derive(Clone, Debug)]
enum Step {
    /// A record field.
    Field(String),
    /// A map key.
    Key(Value),
}

/// The value at `step` inside `v`, if `v` is the matching container and holds it.
fn child<'a>(v: &'a Value, step: &Step) -> Option<&'a Value> {
    match (v, step) {
        (Value::Record(r), Step::Field(f)) => r.get(f),
        (Value::Map(m), Step::Key(k)) => m.iter().find(|(mk, _)| mk == k).map(|(_, v)| v),
        _ => None,
    }
}

/// Every leaf of `v` (a value that is not a record or map) with its path.
fn leaves(v: &Value, path: &mut Vec<Step>, out: &mut Vec<(Vec<Step>, Value)>) {
    let children: Vec<(Step, &Value)> = match v {
        Value::Record(r) => r.iter().map(|(k, v)| (Step::Field(k.clone()), v)).collect(),
        Value::Map(m) => m.iter().map(|(k, v)| (Step::Key(k.clone()), v)).collect(),
        _ => return out.push((path.clone(), v.clone())),
    };
    for (step, c) in children {
        path.push(step);
        leaves(c, path, out);
        path.pop();
    }
}

/// Whether `upper` replaces what `lower` holds at `path`: it holds a value at the path, or at
/// a prefix of it a value that is not a container of the same kind as `lower`'s.
fn shadowed(lower: &Value, upper: &Value, path: &[Step]) -> bool {
    let (mut l, mut u) = (lower, upper);
    for step in path {
        let same_kind = matches!((l, u), (Value::Record(_), Value::Record(_)) | (Value::Map(_), Value::Map(_)));
        if !same_kind {
            return true;
        }
        match (child(l, step), child(u, step)) {
            (Some(lc), Some(uc)) => (l, u) = (lc, uc),
            _ => return false,
        }
    }
    true
}

proptest! {
    /// An empty record is a neutral layer below and above any record.
    #[test]
    fn merge_with_an_empty_record_changes_nothing(v in layer(), w in value()) {
        let mut below = Value::Record(Record::new());
        below.merge(w.clone());
        prop_assert_eq!(below, w);
        if matches!(v, Value::Record(_)) {
            let mut above = v.clone();
            above.merge(Value::Record(Record::new()));
            prop_assert_eq!(above, v);
        }
    }

    /// A layer merged over itself gives itself.
    #[test]
    fn merge_is_idempotent(v in value()) {
        let mut m = v.clone();
        m.merge(v.clone());
        prop_assert_eq!(m, v);
    }

    /// Every leaf of the upper layer is in the result at its path, and every leaf of the lower
    /// layer that the upper layer does not replace is kept.
    #[test]
    fn upper_leaves_win_and_untouched_lower_leaves_stay(lower in layer(), upper in layer()) {
        let mut m = lower.clone();
        m.merge(upper.clone());
        let at = |path: &[Step]| path.iter().try_fold(&m, |v, s| child(v, s));

        let mut ups = Vec::new();
        leaves(&upper, &mut Vec::new(), &mut ups);
        for (path, leaf) in &ups {
            prop_assert_eq!(at(path), Some(leaf), "upper leaf at {:?}", path);
        }

        let mut lows = Vec::new();
        leaves(&lower, &mut Vec::new(), &mut lows);
        for (path, leaf) in lows.iter().filter(|(p, _)| !shadowed(&lower, &upper, p)) {
            prop_assert_eq!(at(path), Some(leaf), "lower leaf at {:?}", path);
        }
    }
}
