//! Kani proofs: the calendar helpers agree and the constructors enforce their ranges.

use super::{Duration, MAX_DURATION, MAX_SECS, MIN_SECS, Timestamp, civil_from_days, days_from_civil, days_in_month, frac_nanos};

/// Seconds per day.
const DAY: i64 = 86_400;

/// For every day in the years 0001-9999, [`civil_from_days`] gives a valid date
/// and [`days_from_civil`] maps it back to the same day.
#[kani::proof]
fn civil_dates_round_trip() {
    let z: i64 = kani::any();
    kani::assume((MIN_SECS / DAY..=MAX_SECS / DAY).contains(&z));
    let (y, m, d) = civil_from_days(z);

    assert!((1..=9999).contains(&y));
    assert!((1..=12).contains(&m));
    assert!(d >= 1 && d <= days_in_month(y, m));
    assert_eq!(days_from_civil(y, m, d), z);
}

/// One to nine ASCII digits always give a nanosecond count below one second.
#[kani::proof]
#[kani::unwind(10)]
fn frac_nanos_stays_below_one_second() {
    let digits: [u8; 9] = kani::any();
    let len: usize = kani::any();
    kani::assume((1..=9).contains(&len));
    kani::assume(digits.iter().all(u8::is_ascii_digit));

    assert!(frac_nanos(&digits[..len]) < 1_000_000_000);
}

/// [`Timestamp::new`] accepts exactly the in-range seconds with a sub-second fraction.
#[kani::proof]
fn timestamp_new_checks_its_range() {
    let secs: i64 = kani::any();
    let nanos: u32 = kani::any();
    let valid = (MIN_SECS..=MAX_SECS).contains(&secs) && nanos < 1_000_000_000;

    assert_eq!(Timestamp::new(secs, nanos).is_ok(), valid);
}

/// [`Duration::from_nanos`] accepts exactly the magnitudes up to `i64::MAX` seconds.
#[kani::proof]
fn duration_from_nanos_checks_its_range() {
    let n: i128 = kani::any();
    let valid = (-MAX_DURATION..=MAX_DURATION).contains(&n);

    assert_eq!(Duration::from_nanos(n).map(|d| d.as_nanos()).ok(), valid.then_some(n));
}
