//! Timestamps (UTC instants) and signed durations.

use std::fmt;

/// An instant on the UTC time line with nanosecond precision.
/// Offsets in the text form are presentation only (spec §2.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp {
    secs: i64,
    nanos: u32,
}

const MIN_SECS: i64 = -62_135_596_800; // 0001-01-01T00:00:00Z
const MAX_SECS: i64 = 253_402_300_799; // 9999-12-31T23:59:59Z

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

impl Timestamp {
    pub fn new(secs: i64, nanos: u32) -> Result<Self, String> {
        if nanos >= 1_000_000_000 || !(MIN_SECS..=MAX_SECS).contains(&secs) {
            return Err("timestamp out of range (years 0001-9999)".into());
        }
        Ok(Timestamp { secs, nanos })
    }

    pub fn unix_seconds(&self) -> i64 {
        self.secs
    }

    pub fn subsec_nanos(&self) -> u32 {
        self.nanos
    }

    /// Parse an RFC 3339 date-time: `YYYY-MM-DDTHH:MM:SS[.F{1,9}](Z|±HH:MM)`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let err = || format!("invalid RFC 3339 timestamp {s:?}");
        let b = s.as_bytes();
        if b.len() < 20 {
            return Err(err());
        }
        let num = |r: std::ops::Range<usize>| -> Result<u32, String> {
            let part = &b[r];
            if !part.iter().all(u8::is_ascii_digit) {
                return Err(err());
            }
            Ok(std::str::from_utf8(part).unwrap().parse().unwrap())
        };
        if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return Err(err());
        }
        let (y, mo, d) = (num(0..4)? as i64, num(5..7)?, num(8..10)?);
        let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
        if y < 1 || !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || se > 59 {
            return Err(err());
        }
        let mut i = 19;
        let mut nanos = 0u32;
        if b[i] == b'.' {
            i += 1;
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n = i - start;
            if n == 0 || n > 9 {
                return Err(err());
            }
            nanos = num(start..i)? * 10u32.pow((9 - n) as u32);
        }
        let offset: i64 = match b.get(i) {
            Some(b'Z') if i + 1 == b.len() => 0,
            Some(c @ (b'+' | b'-')) if i + 6 == b.len() && b[i + 3] == b':' => {
                let oh = num(i + 1..i + 3)? as i64;
                let om = num(i + 4..i + 6)? as i64;
                if oh > 23 || om > 59 {
                    return Err(err());
                }
                let o = oh * 3600 + om * 60;
                if *c == b'-' {
                    -o
                } else {
                    o
                }
            }
            _ => return Err(err()),
        };
        let secs = days_from_civil(y, mo, d) * 86_400 + (h * 3600 + mi * 60 + se) as i64 - offset;
        Timestamp::new(secs, nanos).map_err(|_| err())
    }
}

impl fmt::Display for Timestamp {
    /// Canonical form: UTC, `Z`, fraction trimmed of trailing zeros.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = self.secs.div_euclid(86_400);
        let sod = self.secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        write!(f, "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", sod / 3600, sod / 60 % 60, sod % 60)?;
        if self.nanos != 0 {
            let frac = format!("{:09}", self.nanos);
            write!(f, ".{}", frac.trim_end_matches('0'))?;
        }
        f.write_str("Z")
    }
}

/// A signed, exact span of time in nanoseconds. Days are exactly 86 400 s;
/// there are no calendar units (spec §2.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Duration {
    nanos: i128,
}

const NS: i128 = 1_000_000_000;
const MAX_DURATION: i128 = i64::MAX as i128 * NS;

impl Duration {
    pub fn from_nanos(nanos: i128) -> Result<Self, String> {
        if nanos.abs() > MAX_DURATION {
            return Err("duration out of range".into());
        }
        Ok(Duration { nanos })
    }

    pub fn as_nanos(&self) -> i128 {
        self.nanos
    }

    /// Parse `-?(Nd)?(Nh)?(Nm)?(N(.F{1,9})?s)?` with at least one component.
    pub fn parse(s: &str) -> Result<Self, String> {
        let err = || format!("invalid duration {s:?} (expected e.g. 1h30m or 1.5s)");
        let b = s.as_bytes();
        let mut i = 0;
        let neg = b.first() == Some(&b'-');
        if neg {
            i += 1;
        }
        let units = [(b'd', 86_400 * NS), (b'h', 3_600 * NS), (b'm', 60 * NS), (b's', NS)];
        let mut next_unit = 0;
        let mut total: i128 = 0;
        let mut any = false;
        while i < b.len() {
            let start = i;
            let mut n: i128 = 0;
            while i < b.len() && b[i].is_ascii_digit() {
                n = n.checked_mul(10).and_then(|v| v.checked_add((b[i] - b'0') as i128)).ok_or_else(err)?;
                i += 1;
            }
            if i == start {
                return Err(err());
            }
            let mut frac_ns: i128 = 0;
            if i < b.len() && b[i] == b'.' {
                i += 1;
                let fs = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                let len = i - fs;
                if len == 0 || len > 9 || b.get(i) != Some(&b's') {
                    return Err(err());
                }
                frac_ns = std::str::from_utf8(&b[fs..i]).unwrap().parse::<i128>().unwrap() * 10i128.pow((9 - len) as u32);
            }
            let u = *b.get(i).ok_or_else(err)?;
            let idx = units[next_unit..].iter().position(|(c, _)| *c == u).ok_or_else(err)? + next_unit;
            next_unit = idx + 1;
            total = n
                .checked_mul(units[idx].1)
                .and_then(|v| v.checked_add(frac_ns))
                .and_then(|v| v.checked_add(total))
                .ok_or_else(err)?;
            any = true;
            i += 1;
        }
        if !any {
            return Err(err());
        }
        Duration::from_nanos(if neg { -total } else { total }).map_err(|_| err())
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.nanos == 0 {
            return f.write_str("0s");
        }
        if self.nanos < 0 {
            f.write_str("-")?;
        }
        let mut n = self.nanos.unsigned_abs();
        let ns = NS as u128;
        for (unit, size) in [("d", 86_400 * ns), ("h", 3_600 * ns), ("m", 60 * ns)] {
            if n >= size {
                write!(f, "{}{unit}", n / size)?;
                n %= size;
            }
        }
        if n > 0 {
            write!(f, "{}", n / ns)?;
            if !n.is_multiple_of(ns) {
                let frac = format!("{:09}", n % ns);
                write!(f, ".{}", frac.trim_end_matches('0'))?;
            }
            f.write_str("s")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        let t = Timestamp::parse("2026-11-17T16:00:00Z").unwrap();
        assert_eq!(t.unix_seconds(), 1_794_931_200);
        assert_eq!(t.to_string(), "2026-11-17T16:00:00Z");
        let o = Timestamp::parse("2026-11-17T18:00:00.250+02:00").unwrap();
        assert_eq!(o.to_string(), "2026-11-17T16:00:00.25Z");
        assert_eq!(Timestamp::parse("1970-01-01T00:00:00Z").unwrap().unix_seconds(), 0);
        assert_eq!(Timestamp::parse("0001-01-01T00:00:00Z").unwrap().unix_seconds(), MIN_SECS);
        assert!(Timestamp::parse("2026-02-29T00:00:00Z").is_err());
        assert!(Timestamp::parse("2024-02-29T00:00:00Z").is_ok());
        assert!(Timestamp::parse("2026-11-17 16:00:00Z").is_err());
        assert!(Timestamp::parse("2026-11-17T16:00:00").is_err());
    }

    #[test]
    fn durations() {
        for (s, c) in [
            ("1h30m", "1h30m"),
            ("90m", "1h30m"),
            ("0s", "0s"),
            ("1.5s", "1.5s"),
            ("-2d3h", "-2d3h"),
            ("0.000000001s", "0.000000001s"),
            ("3600s", "1h"),
        ] {
            assert_eq!(Duration::parse(s).unwrap().to_string(), c, "{s}");
        }
        assert!(Duration::parse("1m1h").is_err());
        assert!(Duration::parse("").is_err());
        assert!(Duration::parse("1.5m").is_err());
    }
}
