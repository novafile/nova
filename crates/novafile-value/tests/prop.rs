//! Property tests: the binary codec round-trips and never panics, and the scalar
//! types keep their invariants through parsing, display and comparison.

#[path = "support/strategy.rs"]
mod strategy;

use std::cmp::Ordering;

use novafile_value::{Decimal, Duration, EncodeOptions, Rational, Timestamp, Value, canonical_bytes, decode, decode_ref, encode, numeric_cmp};
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
