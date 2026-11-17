//! Exact numeric comparison across `int`, `float`, `decimal` and `rational`.
//!
//! The kinds stay distinct values (spec §2.5); this is the *numeric order*
//! used by schema `@range` checks and query filters (spec §6.1, §7.1). It is
//! exact: no conversion through binary64. Magnitudes are compared first, so
//! extreme decimal exponents never build huge integers.

use crate::Value;
use num_bigint::{BigInt, Sign};
use std::cmp::Ordering;

enum Num {
    NegInf,
    PosInf,
    /// sign * num / den * 10^exp10, with num, den > 0 (or num == 0).
    Finite {
        num: BigInt,
        den: BigInt,
        exp10: i64,
        approx_log10: f64,
    },
}

#[expect(
    clippy::cast_precision_loss,
    reason = "an approximation is intended; the u64 digit is only cast when it has at most 53 bits"
)]
fn digits_log10(x: &BigInt) -> f64 {
    // log10 of a big integer, approximately (good to well under 1 decade).
    let bits = x.bits() as f64;
    if bits <= 53.0 {
        let (_, d) = x.to_u64_digits();
        let v = d.first().copied().unwrap_or(0) as f64;
        return if v == 0.0 { f64::NEG_INFINITY } else { v.log10() };
    }

    bits * std::f64::consts::LOG10_2
}

fn of(v: &Value) -> Option<Num> {
    Some(match v {
        Value::Int(i) => {
            let n = BigInt::from(*i);
            let approx_log10 = digits_log10(&n);
            Num::Finite {
                num: n,
                den: BigInt::from(1),
                exp10: 0,
                approx_log10,
            }
        }
        Value::Float(f) => {
            if f.is_nan() {
                return None;
            }

            if f.is_infinite() {
                return Some(if *f > 0.0 { Num::PosInf } else { Num::NegInf });
            }

            // f = mantissa * 2^exp exactly.
            let bits = f.to_bits();
            let neg = bits >> 63 == 1;
            // 11-bit exponent field, so the signed reinterpretation is exact.
            let exp_bits = ((bits >> 52) & 0x7FF).cast_signed();
            let frac = bits & ((1u64 << 52) - 1);
            let (mantissa, exp2) = if exp_bits == 0 {
                (frac, -1074)
            } else {
                (frac | (1u64 << 52), exp_bits - 1075)
            };

            let mut n = BigInt::from(mantissa);
            if neg {
                n = -n;
            }

            let (num, den) = if exp2 >= 0 {
                (n << exp2.unsigned_abs(), BigInt::from(1))
            } else {
                (n, BigInt::from(1) << exp2.unsigned_abs())
            };

            Num::Finite {
                num,
                den,
                exp10: 0,
                approx_log10: if *f == 0.0 { f64::NEG_INFINITY } else { f.abs().log10() },
            }
        }
        Value::Decimal(d) => {
            let n = BigInt::from(d.coefficient());
            let approx_log10 = digits_log10(&n) + f64::from(d.exponent());
            Num::Finite {
                num: n,
                den: BigInt::from(1),
                exp10: i64::from(d.exponent()),
                approx_log10,
            }
        }
        Value::Rational(r) => {
            let (n, dd) = (BigInt::from(r.numerator()), BigInt::from(r.denominator()));
            let approx_log10 = digits_log10(&n) - digits_log10(&dd);
            Num::Finite {
                num: n,
                den: dd,
                exp10: 0,
                approx_log10,
            }
        }
        _ => return None,
    })
}

fn sign(n: &BigInt) -> i8 {
    match n.sign() {
        Sign::Minus => -1,
        Sign::NoSign => 0,
        Sign::Plus => 1,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers pass a non-negative exponent gap, bounded to a few dozen by the magnitude check"
)]
fn scale(n: BigInt, exp10: i64) -> BigInt {
    n * BigInt::from(10).pow(exp10 as u32)
}

/// Compare two numeric values exactly. `None` if either is not numeric or is NaN.
#[must_use]
pub fn numeric_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    let (a, b) = (of(a)?, of(b)?);
    let (an, ad, ae, al, bn, bd, be, bl) = match (a, b) {
        (Num::NegInf, Num::NegInf) | (Num::PosInf, Num::PosInf) => return Some(Ordering::Equal),
        (Num::NegInf, _) | (_, Num::PosInf) => return Some(Ordering::Less),
        (_, Num::NegInf) | (Num::PosInf, _) => return Some(Ordering::Greater),
        (
            Num::Finite {
                num: an,
                den: ad,
                exp10: ae,
                approx_log10: al,
            },
            Num::Finite {
                num: bn,
                den: bd,
                exp10: be,
                approx_log10: bl,
            },
        ) => (an, ad, ae, al, bn, bd, be, bl),
    };

    let (sa, sb) = (sign(&an), sign(&bn));
    if sa != sb || sa == 0 {
        return Some(sa.cmp(&sb));
    }

    // Same nonzero sign: if magnitudes differ by more than a decade (with margin), decide directly.
    if (al - bl).abs() > 2.0 {
        let mag = al.partial_cmp(&bl)?;
        return Some(if sa > 0 { mag } else { mag.reverse() });
    }

    // Magnitudes are close, so the decimal exponents differ by a bounded amount.
    let m = ae.min(be);
    let (an, bn) = (scale(an, ae - m), scale(bn, be - m));
    Some((an * bd).cmp(&(bn * ad)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Decimal, Rational};

    fn d(s: &str) -> Value {
        Value::Decimal(Decimal::parse(s).unwrap())
    }

    fn r(n: i128, dd: i128) -> Value {
        Value::Rational(Rational::new(n, dd).unwrap())
    }

    #[test]
    fn exact_across_kinds() {
        use Ordering::*;
        let cases: Vec<(Value, Value, Ordering)> = vec![
            (Value::Int(3), Value::Float(3.0), Equal),
            (Value::Float(0.1), d("0.1"), Greater), // binary64 0.1 is slightly above 1/10
            (Value::Float(0.5), d("0.5"), Equal),
            (r(1, 3), d("0.3333333333333333333333333333333333333"), Greater),
            (r(1, 3), Value::Float(1.0 / 3.0), Greater),
            (r(1, 2), Value::Float(0.5), Equal),
            (Value::Int(i128::MAX), Value::Float(1.701_411_834_604_692_3e38), Less), // float rounds up past i128::MAX
            (Value::Int(9_007_199_254_740_993), Value::Float(9_007_199_254_740_992.0), Greater), // 2^53+1 vs 2^53, equal in binary64
            (d("1e400"), Value::Float(f64::MAX), Greater),
            (d("-1e400"), Value::Float(f64::MIN), Less),
            (d("1e-400"), Value::Float(0.0), Greater),
            (d("12e1000000000"), d("1.2e1000000001"), Equal),
            (d("12e1000000000"), d("13e1000000000"), Less),
            (Value::Float(-0.0), Value::Int(0), Equal),
            (Value::Float(f64::INFINITY), d("1e999999999"), Greater),
            (Value::Int(-5), r(-11, 2), Greater),
        ];
        for (a, b, want) in cases {
            assert_eq!(numeric_cmp(&a, &b), Some(want), "{a:?} vs {b:?}");
            assert_eq!(numeric_cmp(&b, &a), Some(want.reverse()), "{b:?} vs {a:?}");
        }

        assert_eq!(numeric_cmp(&Value::Float(f64::NAN), &Value::Int(1)), None);
        assert_eq!(numeric_cmp(&Value::String("1".into()), &Value::Int(1)), None);
    }
}
