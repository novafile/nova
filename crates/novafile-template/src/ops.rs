//! Operators, ordering, iteration order and exact numeric helpers (proposal 0006
//! §0006.2.3, §0006.5.4, §0006.6).

use novafile_text::ast::BinOp;
use novafile_value::{Decimal, Duration, Rational, Timestamp, Value, canonical_bytes, numeric_cmp};
use num_bigint::{BigInt, Sign};
use std::cmp::Ordering;

/// Smallest integer every implementation must hold: −2^63.
const INT_MIN: i128 = -(1 << 63);
/// Largest integer every implementation must hold: 2^64 − 1.
const INT_MAX: i128 = (1 << 64) - 1;
/// Nanoseconds per second.
const NS: i128 = 1_000_000_000;

/// The interpolation text of a scalar (spec §9.3): a string as is, a unit variant as its tag,
/// any other number, bool, timestamp or duration as its canonical text. `None` for `null`,
/// bytes, containers and variants with a payload.
pub(crate) fn scalar_text(v: &Value) -> Option<String> {
    Some(match v {
        Value::String(x) => x.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f:?}"),
        Value::Decimal(d) => d.to_string(),
        Value::Rational(r) => r.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Timestamp(t) => t.to_string(),
        Value::Duration(d) => d.to_string(),
        Value::Variant(t, None) => t.clone(),
        _ => return None,
    })
}

/// The `(index or key, element)` pairs of a collection in iteration order (§0006.6): lists
/// and tuples in order with their index, records by key in UTF-8 byte order, maps by the
/// canonical order of their keys, sets in canonical order with their position. `None` for
/// anything else.
pub(crate) fn iterate(v: Value) -> Option<Vec<(Value, Value)>> {
    /// Pair each element with its 0-based position.
    fn indexed(l: Vec<Value>) -> Vec<(Value, Value)> {
        l.into_iter().enumerate().map(|(i, v)| (Value::Int(i as i128), v)).collect()
    }

    Some(match v {
        Value::List(l) | Value::Tuple(l) => indexed(l),
        Value::Set(mut l) => {
            l.sort_by_cached_key(canonical_bytes);
            indexed(l)
        }
        Value::Record(r) => {
            let mut e = r.into_entries();
            e.sort_by(|a, b| a.0.cmp(&b.0));
            e.into_iter().map(|(k, v)| (Value::String(k), v)).collect()
        }
        Value::Map(mut m) => {
            m.sort_by_cached_key(|(k, _)| canonical_bytes(k));
            m
        }
        _ => return None,
    })
}

/// Order two values of the same ordered kind (§0006.2.3): numbers of one kind numerically,
/// strings by code point, timestamps by instant, durations by length, `false` before `true`.
pub(crate) fn order(a: &Value, b: &Value) -> Result<Ordering, String> {
    Ok(match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Decimal(_), Value::Decimal(_)) | (Value::Rational(_), Value::Rational(_)) => numeric_cmp(a, b).expect("numbers"),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).ok_or("cannot order NaN")?,
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Timestamp(x), Value::Timestamp(y)) => x.cmp(y),
        (Value::Duration(x), Value::Duration(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ if a.kind() == b.kind() => return Err(format!("{} values are not ordered", a.kind())),
        _ => return Err(format!("cannot compare a {} with a {}; kinds never convert implicitly", a.kind(), b.kind())),
    })
}

/// Apply a comparison or arithmetic operator (not `and`/`or`, which short-circuit).
pub(crate) fn binary(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    let ord = |want: fn(Ordering) -> bool| order(a, b).map(|o| Value::Bool(want(o)));
    match op {
        BinOp::Eq => Ok(Value::Bool(a == b)),
        BinOp::Ne => Ok(Value::Bool(a != b)),
        BinOp::Lt => ord(Ordering::is_lt),
        BinOp::Le => ord(Ordering::is_le),
        BinOp::Gt => ord(Ordering::is_gt),
        BinOp::Ge => ord(Ordering::is_ge),
        BinOp::Add | BinOp::Sub | BinOp::Mul => arith(op, a, b),
        BinOp::And | BinOp::Or => unreachable!("short-circuit operators are evaluated by the caller"),
    }
}

/// `+`, `-` or `*` by the arithmetic table of §0006.2.3; every other combination is an error.
#[expect(clippy::many_single_char_names, reason = "short names for operands, as in the formulas they implement")]
fn arith(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    use BinOp::{Add, Mul, Sub};
    match (op, a, b) {
        (_, Value::Int(x), Value::Int(y)) => int(match op {
            Add => x.checked_add(*y),
            Sub => x.checked_sub(*y),
            _ => x.checked_mul(*y),
        }),
        (_, Value::Decimal(x), Value::Decimal(y)) => decimal_op(op, *x, *y),
        (_, Value::Rational(x), Value::Rational(y)) => {
            let (n1, d1, n2, d2) = (big(x.numerator()), big(x.denominator()), big(y.numerator()), big(y.denominator()));
            match op {
                Add => rational(&n1 * &d2 + &n2 * &d1, d1 * d2),
                Sub => rational(&n1 * &d2 - &n2 * &d1, d1 * d2),
                _ => rational(n1 * n2, d1 * d2),
            }
        }
        (_, Value::Float(x), Value::Float(y)) => Ok(Value::Float(match op {
            Add => x + y,
            Sub => x - y,
            _ => x * y,
        })),
        (Add | Sub, Value::Timestamp(t), Value::Duration(d)) => {
            let n = d.as_nanos();
            timestamp(instant(t) + if op == Add { n } else { -n })
        }
        (Sub, Value::Timestamp(x), Value::Timestamp(y)) => duration(Some(instant(x) - instant(y))),
        (Add, Value::Duration(x), Value::Duration(y)) => duration(x.as_nanos().checked_add(y.as_nanos())),
        (Sub, Value::Duration(x), Value::Duration(y)) => duration(x.as_nanos().checked_sub(y.as_nanos())),
        (Mul, Value::Duration(d), Value::Int(i)) => duration(d.as_nanos().checked_mul(*i)),
        _ => Err(format!(
            "cannot apply `{}` to a {} and a {}; kinds never convert implicitly",
            op.as_str(),
            a.kind(),
            b.kind()
        )),
    }
}

/// An integer result, checked against the portable range [−2^63, 2^64 − 1].
pub(crate) fn int(r: Option<i128>) -> Result<Value, String> {
    r.filter(|i| (INT_MIN..=INT_MAX).contains(i))
        .map(Value::Int)
        .ok_or_else(|| "integer result is outside the portable range [-2^63, 2^64 - 1]".into())
}

/// The big integer `i`.
fn big(i: i128) -> BigInt {
    BigInt::from(i)
}

/// An integer result given as a big integer, checked like [`int`].
fn int_big(n: &BigInt) -> Result<Value, String> {
    int(i128::try_from(n).ok())
}

/// Nanoseconds since the Unix epoch.
fn instant(t: &Timestamp) -> i128 {
    i128::from(t.unix_seconds()) * NS + i128::from(t.subsec_nanos())
}

/// The timestamp `n` nanoseconds after the Unix epoch, if in years 0001–9999.
fn timestamp(n: i128) -> Result<Value, String> {
    let secs = i64::try_from(n.div_euclid(NS)).map_err(|_| "timestamp out of range (years 0001-9999)")?;
    let nanos = u32::try_from(n.rem_euclid(NS)).expect("below one second");
    Timestamp::new(secs, nanos).map(Value::Timestamp)
}

/// A duration result, checked against ±(2^63 − 1) seconds.
fn duration(n: Option<i128>) -> Result<Value, String> {
    Duration::from_nanos(n.ok_or("duration out of range")?).map(Value::Duration)
}

/// Exact `+`, `-` or `*` of decimals; the result must have at most 38 significant digits and
/// a 32-bit exponent (spec §2.3).
fn decimal_op(op: BinOp, x: Decimal, y: Decimal) -> Result<Value, String> {
    let (cx, ex, cy, ey) = (x.coefficient(), i64::from(x.exponent()), y.coefficient(), i64::from(y.exponent()));
    if op == BinOp::Mul {
        return decimal(big(cx) * big(cy), ex + ey);
    }

    let cy = if op == BinOp::Sub { -cy } else { cy };
    if cx == 0 {
        return decimal(big(cy), ey);
    }

    if cy == 0 {
        return Ok(Value::Decimal(x));
    }

    let e = ex.min(ey);
    // Both are nonzero with at most 39 digits, so the exact sum needs more than 38 digits
    // once the exponents are this far apart.
    if ex.max(ey) - e > 80 {
        return Err("decimal result has more than 38 significant digits".into());
    }

    let scale = |c: i128, ec: i64| big(c) * BigInt::from(10).pow(u32::try_from(ec - e).expect("small"));
    decimal(scale(cx, ex) + scale(cy, ey), e)
}

/// The decimal `c × 10^e`, normalized and checked against the limits of spec §2.3.
fn decimal(mut c: BigInt, mut e: i64) -> Result<Value, String> {
    let ten = BigInt::from(10);
    if c.sign() == Sign::NoSign {
        return Ok(Value::Decimal(Decimal::new(0, 0)));
    }

    while (&c % &ten).sign() == Sign::NoSign {
        c /= &ten;
        e += 1;
    }

    if c.magnitude().to_string().len() > 38 {
        return Err("decimal result has more than 38 significant digits".into());
    }

    let e = i32::try_from(e).map_err(|_| "decimal result exponent is out of range")?;
    Ok(Value::Decimal(Decimal::new(i128::try_from(&c).expect("38 digits fit"), e)))
}

/// Greatest common divisor of two non-negative big integers.
fn gcd(mut a: BigInt, mut b: BigInt) -> BigInt {
    while b.sign() != Sign::NoSign {
        let r = &a % &b;
        a = b;
        b = r;
    }

    a
}

/// The rational `n/d` (`d > 0`) in lowest terms; the numerator must fit in a signed and the
/// denominator in an unsigned 64-bit integer (spec §2.4).
fn rational(n: BigInt, d: BigInt) -> Result<Value, String> {
    let g = gcd(BigInt::from_biguint(Sign::Plus, n.magnitude().clone()), d.clone()).max(BigInt::from(1));
    let (n, d) = (n / &g, d / g);
    match (i64::try_from(&n), u64::try_from(&d)) {
        (Ok(n), Ok(d)) => Rational::new(i128::from(n), i128::from(d)).map(Value::Rational),
        _ => Err("rational result does not fit in 64-bit numerator and denominator".into()),
    }
}

/// How [`to_int`] rounds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Round {
    /// Toward −∞.
    Floor,
    /// Toward +∞.
    Ceil,
    /// To the nearest integer, ties away from zero.
    Nearest,
}

/// Round the fraction `n/d` (`d > 0`) to an integer.
#[expect(clippy::many_single_char_names, reason = "short names for operands, as in the formulas they implement")]
fn round_frac(n: &BigInt, d: &BigInt, mode: Round) -> BigInt {
    let (q, r) = (n / d, n % d);
    let exact = r.sign() == Sign::NoSign;
    let floor = if !exact && n.sign() == Sign::Minus { q - 1 } else { q };
    match mode {
        Round::Floor => floor,
        Round::Ceil if exact => floor,
        Round::Ceil => floor + 1,
        Round::Nearest => {
            // sign(n) × floor((2|n| + d) / 2d)
            let m = BigInt::from_biguint(Sign::Plus, n.magnitude().clone());
            let k: BigInt = (m * 2 + d) / (d * 2);
            if n.sign() == Sign::Minus { -k } else { k }
        }
    }
}

/// `floor`, `ceil` or `round` of a decimal, rational or float, as a portable integer.
#[expect(clippy::many_single_char_names, reason = "short names for operands, as in the formulas they implement")]
pub(crate) fn to_int(v: &Value, mode: Round) -> Result<Value, String> {
    match v {
        Value::Float(f) => {
            if !f.is_finite() {
                return Err("cannot round a non-finite float".into());
            }

            let r = match mode {
                Round::Floor => f.floor(),
                Round::Ceil => f.ceil(),
                Round::Nearest => f.round(),
            };

            #[expect(clippy::cast_precision_loss, reason = "both bounds are powers of two, exact in binary64")]
            let ok = r >= INT_MIN as f64 && r < (INT_MAX + 1) as f64;
            #[expect(clippy::cast_possible_truncation, reason = "integral and range-checked above")]
            let i = r as i128;
            int(ok.then_some(i))
        }
        Value::Rational(q) => int_big(&round_frac(&big(q.numerator()), &big(q.denominator()), mode)),
        Value::Decimal(d) => {
            let (c, e) = (d.coefficient(), d.exponent());
            if e >= 0 {
                // A nonzero coefficient times 10^40 is beyond the portable range.
                let p = u32::try_from(e)
                    .ok()
                    .filter(|&p| p <= 40)
                    .ok_or("integer result is outside the portable range [-2^63, 2^64 - 1]")?;
                return int_big(&(big(c) * BigInt::from(10).pow(p)));
            }

            if e < -40 {
                // |c| < 10^39, so |d| < 0.1.
                return int(Some(match (mode, c.signum()) {
                    (Round::Floor, -1) => -1,
                    (Round::Ceil, 1) => 1,
                    _ => 0,
                }));
            }

            int_big(&round_frac(&big(c), &BigInt::from(10).pow(e.unsigned_abs()), mode))
        }
        other => Err(format!("expected a decimal, rational or float, found {}", other.kind())),
    }
}

/// The binary64 value nearest to `n/d` (`d > 0`), ties to even, rounding only once.
#[expect(clippy::many_single_char_names, reason = "short names for operands, as in the formulas they implement")]
pub(crate) fn ratio_to_f64(n: i128, d: i128) -> f64 {
    if n == 0 {
        return 0.0;
    }

    let (num, den) = (BigInt::from(n.unsigned_abs()), BigInt::from(d));
    // Scale so the integer quotient has 66 or 67 bits: more than binary64's 53 plus guard bits.
    let shift = 66 - (i64::try_from(num.bits()).expect("bits") - i64::try_from(den.bits()).expect("bits"));
    let (num, den) = if shift >= 0 {
        (num << shift.unsigned_abs(), den)
    } else {
        (num, den << shift.unsigned_abs())
    };

    let q = u128::try_from(&num / &den).expect("at most 67 bits");
    // A nonzero remainder becomes a sticky bit below the rounding position.
    let sticky = u128::from((num % den).sign() != Sign::NoSign);
    #[expect(clippy::cast_precision_loss, reason = "the one correctly rounded conversion")]
    let f = (q | sticky) as f64;
    // Multiplying by a power of two is exact here: the magnitudes stay far from subnormals.
    let e = i32::try_from(-shift).expect("small exponent");
    let r = f * 2f64.powi(e);
    if n < 0 { -r } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_modes() {
        let d = |s| Value::Decimal(Decimal::parse(s).unwrap());
        let r = |n, d| Value::Rational(Rational::new(n, d).unwrap());
        for (v, f, c, n) in [
            (d("2.5"), 2, 3, 3),
            (d("-2.5"), -3, -2, -3),
            (r(-7, 2), -4, -3, -4),
            (r(7, 3), 2, 3, 2),
            (Value::Float(-0.5), -1, 0, -1),
            (d("1e-50"), 0, 1, 0),
            (d("-1e-50"), -1, 0, 0),
        ] {
            assert_eq!(to_int(&v, Round::Floor), Ok(Value::Int(f)), "{v:?}");
            assert_eq!(to_int(&v, Round::Ceil), Ok(Value::Int(c)), "{v:?}");
            assert_eq!(to_int(&v, Round::Nearest), Ok(Value::Int(n)), "{v:?}");
        }

        assert!(to_int(&d("1e40"), Round::Floor).is_err());
        assert!(to_int(&Value::Float(f64::NAN), Round::Floor).is_err());
    }

    #[test]
    fn ratio_rounds_once() {
        assert_eq!(ratio_to_f64(1, 3), 1.0 / 3.0);
        assert_eq!(ratio_to_f64(-1, 10), -0.1);
        // 2^53 + 1 is not a binary64; the quotient by 1 rounds to even.
        assert_eq!(ratio_to_f64((1 << 53) + 1, 1), 9_007_199_254_740_992.0);
        let want: f64 = "3074457345618258602.3333333333333333".parse().unwrap();
        assert_eq!(ratio_to_f64((1 << 63) - 1, 3), want);
    }

    #[test]
    fn decimal_limits() {
        let d = |s| Value::Decimal(Decimal::parse(s).unwrap());
        assert_eq!(binary(BinOp::Add, &d("0.1"), &d("0.2")), Ok(d("0.3")));
        assert_eq!(binary(BinOp::Mul, &d("1.5"), &d("-2")), Ok(d("-3")));
        assert!(binary(BinOp::Add, &d("1e100"), &d("1e-100")).is_err());
        assert!(binary(BinOp::Mul, &d("12345678901234567891"), &d("12345678901234567891")).is_err());
    }
}
