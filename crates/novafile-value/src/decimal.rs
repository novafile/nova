//! Exact decimal numbers: `coefficient × 10^exponent`, always normalized.

use std::fmt;

/// An exact decimal number. Normalized so that the coefficient has no
/// trailing decimal zeros; zero is always `0 × 10^0`. Consequently
/// `1.50` and `1.5` are the same value (spec §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "facet", derive(facet::Facet), facet(opaque, proxy = String))]
pub struct Decimal {
    coeff: i128,
    exp: i32,
}

impl Decimal {
    #[must_use]
    pub fn new(mut coeff: i128, mut exp: i32) -> Self {
        if coeff == 0 {
            return Decimal { coeff: 0, exp: 0 };
        }

        while coeff % 10 == 0 {
            coeff /= 10;
            exp = exp.saturating_add(1);
        }

        Decimal { coeff, exp }
    }

    #[must_use]
    pub fn coefficient(&self) -> i128 {
        self.coeff
    }

    #[must_use]
    pub fn exponent(&self) -> i32 {
        self.exp
    }

    #[must_use]
    pub fn from_int(i: i128) -> Self {
        Decimal::new(i, 0)
    }

    /// Parse the decimal lexical form: `-?D+(.D+)?([eE][+-]?D+)?`.
    ///
    /// # Errors
    ///
    /// Returns an error if `s` does not match that form, has more than 38
    /// significant digits, or its exponent does not fit in `i32`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let err = || format!("invalid decimal literal {s:?}");
        let b = s.as_bytes();
        let mut i = 0;
        let neg = b.first() == Some(&b'-');
        if neg {
            i += 1;
        }

        let mut coeff: i128 = 0;
        let mut digits = 0usize;
        let mut frac = 0i64;
        let push = |c: u8, coeff: &mut i128| -> Result<(), String> {
            *coeff = coeff
                .checked_mul(10)
                .and_then(|v| v.checked_add(i128::from(c - b'0')))
                .ok_or_else(|| format!("decimal {s:?} exceeds 38 significant digits"))?;
            Ok(())
        };

        while i < b.len() && b[i].is_ascii_digit() {
            push(b[i], &mut coeff)?;
            digits += 1;
            i += 1;
        }

        if digits == 0 {
            return Err(err());
        }

        if i < b.len() && b[i] == b'.' {
            i += 1;
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                push(b[i], &mut coeff)?;
                frac += 1;
                i += 1;
            }

            if i == start {
                return Err(err());
            }
        }

        let mut e: i64 = 0;
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            i += 1;
            let eneg = match b.get(i) {
                Some(b'-') => {
                    i += 1;
                    true
                }
                Some(b'+') => {
                    i += 1;
                    false
                }
                _ => false,
            };

            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                e = e * 10 + i64::from(b[i] - b'0');
                if e > i64::from(i32::MAX) {
                    return Err(err());
                }

                i += 1;
            }

            if i == start {
                return Err(err());
            }

            if eneg {
                e = -e;
            }
        }

        if i != b.len() {
            return Err(err());
        }

        let exp = i32::try_from(e - frac).map_err(|_| err())?;
        Ok(Decimal::new(if neg { -coeff } else { coeff }, exp))
    }

    #[must_use]
    pub fn to_f64(&self) -> f64 {
        format!("{}e{}", self.coeff, self.exp).parse().unwrap_or(f64::NAN)
    }

    /// Exact conversion to an integer if the value is integral and fits.
    #[must_use]
    pub fn to_i128(&self) -> Option<i128> {
        if self.exp < 0 {
            return None;
        }

        let mut v = self.coeff;
        for _ in 0..self.exp {
            v = v.checked_mul(10)?;
        }

        Some(v)
    }
}

impl fmt::Display for Decimal {
    /// Canonical decimal text (spec §4.6).
    #[expect(
        clippy::cast_possible_wrap,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "at most 40 digits; each usize cast is guarded by a range check to a small non-negative value"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let neg = self.coeff < 0;
        let digits = self.coeff.unsigned_abs().to_string();
        let sign = if neg { "-" } else { "" };
        let exp = i64::from(self.exp);
        let n = digits.len() as i64;
        if (0..=6).contains(&exp) {
            write!(f, "{sign}{digits}{}", "0".repeat(exp as usize))
        } else if exp < 0 && n + exp >= -5 {
            if n + exp > 0 {
                let (a, b) = digits.split_at((n + exp) as usize);
                write!(f, "{sign}{a}.{b}")
            } else {
                write!(f, "{sign}0.{}{digits}", "0".repeat((-(n + exp)) as usize))
            }
        } else {
            write!(f, "{sign}{digits}e{exp}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_formats() {
        assert_eq!(Decimal::parse("1.50").unwrap(), Decimal::parse("1.5").unwrap());
        assert_eq!(Decimal::parse("1.50").unwrap().to_string(), "1.5");
        assert_eq!(Decimal::parse("-0.00").unwrap().to_string(), "0");
        assert_eq!(Decimal::parse("1500").unwrap().to_string(), "1500");
        assert_eq!(Decimal::parse("0.0001").unwrap().to_string(), "0.0001");
        assert_eq!(Decimal::parse("1e30").unwrap().to_string(), "1e30");
        assert_eq!(Decimal::parse("1.2e-20").unwrap().to_string(), "12e-21");
        for s in ["12.5", "-3", "0.000123", "123456789.123456789", "7e-30", "4e12"] {
            let d = Decimal::parse(s).unwrap();
            assert_eq!(Decimal::parse(&d.to_string()).unwrap(), d, "{s}");
        }

        assert!(Decimal::parse(".5").is_err());
        assert!(Decimal::parse("1.").is_err());
        assert!(Decimal::parse("1e").is_err());
    }
}
