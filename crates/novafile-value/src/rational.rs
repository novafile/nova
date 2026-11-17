//! Exact rational numbers `n/d`, always normalized (spec §2.4).

use std::fmt;

/// An exact fraction in lowest terms with a positive denominator, so
/// `2/4` and `1/2` are the same value. `n/1` is still a rational, distinct
/// from the integer `n`, just as decimals are distinct from integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "facet", derive(facet::Facet), facet(opaque, proxy = String))]
pub struct Rational {
    num: i128,
    den: i128,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Rational {
    /// Build a normalized rational `num/den`.
    ///
    /// # Errors
    ///
    /// Returns an error if `den` is zero or either component is `i128::MIN`.
    pub fn new(num: i128, den: i128) -> Result<Self, String> {
        if den == 0 {
            return Err("rational with zero denominator".into());
        }
        if num == i128::MIN || den == i128::MIN {
            return Err("rational component out of range".into());
        }
        // Both magnitudes are at most i128::MAX (MIN is rejected above), so the gcd is too.
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()).max(1).cast_signed();
        let (mut n, mut d) = (num / g, den / g);
        if d < 0 {
            n = -n;
            d = -d;
        }
        Ok(Rational { num: n, den: d })
    }

    #[must_use]
    pub fn numerator(&self) -> i128 {
        self.num
    }

    #[must_use]
    pub fn denominator(&self) -> i128 {
        self.den
    }

    /// Parse `-?N/D` where N and D are decimal integers without leading zeros and D > 0.
    ///
    /// # Errors
    ///
    /// Returns an error if `s` does not have that form, a component does not fit
    /// in `i128`, or the denominator is zero.
    pub fn parse(s: &str) -> Result<Self, String> {
        let err = || format!("invalid rational {s:?} (expected e.g. 22/7)");
        let (n, d) = s.split_once('/').ok_or_else(err)?;
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit()) && (t == "0" || !t.starts_with('0'));
        let nd = n.strip_prefix('-').unwrap_or(n);
        if !digits(nd) || !digits(d) {
            return Err(err());
        }
        let num: i128 = n.parse().map_err(|_| err())?;
        let den: i128 = d.parse().map_err(|_| err())?;
        Rational::new(num, den)
    }

    #[must_use]
    #[expect(clippy::cast_precision_loss, reason = "nearest binary64 approximation is the intent")]
    pub fn to_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// The exact decimal value, if the denominator has only the factors 2 and 5.
    #[must_use]
    pub fn to_decimal(&self) -> Option<crate::Decimal> {
        let mut d = self.den;
        let (mut twos, mut fives) = (0u32, 0u32);
        while d % 2 == 0 {
            d /= 2;
            twos += 1;
        }
        while d % 5 == 0 {
            d /= 5;
            fives += 1;
        }
        if d != 1 {
            return None;
        }
        let k = twos.max(fives);
        // num/den = num * 2^(k-twos) * 5^(k-fives) / 10^k
        let scale = 2i128.checked_pow(k - twos)?.checked_mul(5i128.checked_pow(k - fives)?)?;
        Some(crate::Decimal::new(self.num.checked_mul(scale)?, -i32::try_from(k).ok()?))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(Rational::parse("2/4").unwrap(), Rational::parse("1/2").unwrap());
        assert_eq!(Rational::new(3, -6).unwrap().to_string(), "-1/2");
        assert_eq!(Rational::parse("0/5").unwrap().to_string(), "0/1");
        assert_eq!(Rational::parse("22/7").unwrap().to_string(), "22/7");
        for bad in ["1/0", "01/2", "1/02", "1/-2", "/2", "1/", "a/b", "1.5/2"] {
            assert!(Rational::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Rational::parse("3/8").unwrap().to_decimal().unwrap().to_string(), "0.375");
        assert!(Rational::parse("1/3").unwrap().to_decimal().is_none());
    }
}
