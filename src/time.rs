//! Times and fixed-point numbers, as both file formats store them.
//!
//! * Every record's `time` is a .NET tick count: 100-nanosecond units since
//!   0001-01-01 00:00 UTC. [`TICKS_AT_UNIX_EPOCH`] converts to Unix time.
//! * The order log's BLOCK and META records also carry Unix nanoseconds.
//! * Prices, sizes and positions are integers scaled by [`SCALE`] (10⁸),
//!   exact. Keep them as integers for sums and comparisons; [`to_f64`] and
//!   [`format_fixed`] are for display.
//!
//! [`UtcDateTime`] turns ticks into a calendar date and time and back,
//! without a date-time dependency.

use std::fmt;

/// Prices, sizes and positions are stored as integers times `SCALE`.
pub const SCALE: i64 = 100_000_000;

/// Ticks (100 ns since 0001-01-01 UTC) at 1970-01-01 00:00 UTC.
pub const TICKS_AT_UNIX_EPOCH: i64 = 621_355_968_000_000_000;

/// Ticks in one second.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// Ticks in one day.
pub const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SECOND;

/// Days from 0001-01-01 to 1970-01-01.
const DAYS_TO_UNIX_EPOCH: i64 = TICKS_AT_UNIX_EPOCH / TICKS_PER_DAY;

/// A record time as nanoseconds since 1970-01-01 UTC.
///
/// Unix nanoseconds in an `i64` cover the years 1678 to 2261; times outside
/// that range saturate.
pub fn ticks_to_unix_ns(ticks: i64) -> i64 {
    ticks
        .saturating_sub(TICKS_AT_UNIX_EPOCH)
        .saturating_mul(100)
}

/// Nanoseconds since 1970-01-01 UTC as a record time (rounded down to the tick).
pub fn unix_ns_to_ticks(ns: i64) -> i64 {
    ns.div_euclid(100).saturating_add(TICKS_AT_UNIX_EPOCH)
}

/// A scaled integer (price, size, position) as a float. Keep the integer for
/// exact arithmetic.
pub fn to_f64(value: i64) -> f64 {
    value as f64 / SCALE as f64
}

/// A scaled integer written out exactly, without trailing zeros:
/// `123_110_000_000` is `"1231.1"`, `-50_000_000` is `"-0.5"`.
pub fn format_fixed(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs();
    let scale = SCALE as u64;
    let (whole, fraction) = (abs / scale, abs % scale);
    if fraction == 0 {
        return format!("{sign}{whole}");
    }
    let digits = format!("{fraction:08}");
    format!("{sign}{whole}.{}", digits.trim_end_matches('0'))
}

/// A date and time in UTC, to the nanosecond.
///
/// Made with [`UtcDateTime::new`] (which checks every field) or from a time
/// ([`UtcDateTime::from_ticks`], [`UtcDateTime::from_unix_ns`]); the fields
/// are read with their getters.
///
/// ```
/// use alphaquick::time::UtcDateTime;
///
/// let moment = UtcDateTime::new(2026, 11, 1, 12, 30, 15, 250_000_000).unwrap();
/// let ticks = moment.to_ticks();
/// assert_eq!(UtcDateTime::from_ticks(ticks), moment);
/// assert_eq!(moment.to_string(), "2026-11-01 12:30:15.250000");
/// assert_eq!(format!("{moment:.3}"), "2026-11-01 12:30:15.250");
/// ```
///
/// `Display` writes `YYYY-MM-DD HH:MM:SS` and six decimals of the second;
/// a precision (`{:.3}`, `{:.0}` to `{:.9}`) sets the number of decimals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDateTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    nanosecond: u32,
}

impl UtcDateTime {
    /// A date and time, or `None` when a field is out of range. The year must
    /// be between 1 and 9999.
    pub fn new(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        nanosecond: u32,
    ) -> Option<Self> {
        let valid = (1..=9999).contains(&year)
            && (1..=12).contains(&month)
            && day >= 1
            && day <= days_in_month(year, month)
            && hour < 24
            && minute < 60
            && second < 60
            && nanosecond < 1_000_000_000;
        valid.then_some(UtcDateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
            nanosecond,
        })
    }

    /// Midnight at the start of a day given as the number `YYYYMMDD`, the way
    /// META records and file names write it (`20261101`).
    pub fn from_yyyymmdd(day: u32) -> Option<Self> {
        let year = i32::try_from(day / 10_000).ok()?;
        UtcDateTime::new(year, day / 100 % 100, day % 100, 0, 0, 0, 0)
    }

    /// The day as the number `YYYYMMDD`. Years outside 0 to 9999 do not fit
    /// the form and give a meaningless number.
    pub fn yyyymmdd(&self) -> u32 {
        (self.year.max(0) as u32) * 10_000 + self.month * 100 + self.day
    }

    /// The date and time of a record time (ticks since 0001-01-01 UTC).
    pub fn from_ticks(ticks: i64) -> Self {
        let days = ticks.div_euclid(TICKS_PER_DAY);
        let rest = ticks.rem_euclid(TICKS_PER_DAY);
        let (year, month, day) = civil_from_days(days - DAYS_TO_UNIX_EPOCH);
        let seconds = rest / TICKS_PER_SECOND;
        UtcDateTime {
            year: year as i32,
            month,
            day,
            hour: (seconds / 3600) as u32,
            minute: (seconds / 60 % 60) as u32,
            second: (seconds % 60) as u32,
            nanosecond: (rest % TICKS_PER_SECOND * 100) as u32,
        }
    }

    /// The date and time of a Unix time in nanoseconds (the order log's
    /// block times).
    pub fn from_unix_ns(ns: i64) -> Self {
        let seconds = ns.div_euclid(1_000_000_000);
        let nanosecond = ns.rem_euclid(1_000_000_000) as u32;
        let mut moment = UtcDateTime::from_ticks(
            TICKS_AT_UNIX_EPOCH.saturating_add(seconds.saturating_mul(TICKS_PER_SECOND)),
        );
        moment.nanosecond = nanosecond;
        moment
    }

    /// The year. [`UtcDateTime::new`] accepts 1 to 9999; dates made from
    /// extreme tick counts can lie outside that range.
    pub fn year(&self) -> i32 {
        self.year
    }

    /// The month, 1 to 12.
    pub fn month(&self) -> u32 {
        self.month
    }

    /// The day of the month, 1 to 31.
    pub fn day(&self) -> u32 {
        self.day
    }

    /// The hour, 0 to 23.
    pub fn hour(&self) -> u32 {
        self.hour
    }

    /// The minute, 0 to 59.
    pub fn minute(&self) -> u32 {
        self.minute
    }

    /// The second, 0 to 59.
    pub fn second(&self) -> u32 {
        self.second
    }

    /// Nanoseconds into the second, 0 to 999,999,999.
    pub fn nanosecond(&self) -> u32 {
        self.nanosecond
    }

    /// Midnight at the start of the same day.
    pub fn start_of_day(&self) -> Self {
        UtcDateTime {
            hour: 0,
            minute: 0,
            second: 0,
            nanosecond: 0,
            ..*self
        }
    }

    /// Nanoseconds since 1970-01-01 UTC, exact, in an `i128`.
    fn unix_ns_wide(&self) -> i128 {
        let days = i128::from(days_from_civil(i64::from(self.year), self.month, self.day));
        let seconds = days * 86_400
            + i128::from(self.hour) * 3600
            + i128::from(self.minute) * 60
            + i128::from(self.second);
        seconds * 1_000_000_000 + i128::from(self.nanosecond)
    }

    /// As a record time: ticks since 0001-01-01 UTC, rounded down to 100 ns
    /// (saturating at the limits of `i64`).
    pub fn to_ticks(&self) -> i64 {
        let ticks = self.unix_ns_wide().div_euclid(100) + i128::from(TICKS_AT_UNIX_EPOCH);
        ticks.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// As nanoseconds since 1970-01-01 UTC (saturating outside 1678 to 2261).
    pub fn to_unix_ns(&self) -> i64 {
        self.unix_ns_wide()
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }
}

impl fmt::Display for UtcDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )?;
        let decimals = f.precision().unwrap_or(6).min(9);
        if decimals > 0 {
            let nanos = format!("{:09}", self.nanosecond);
            write!(f, ".{}", &nanos[..decimals])?;
        }
        Ok(())
    }
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a day number since 1970-01-01 (the inverse of `days_from_civil`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = (if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    }) as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_unix_time_and_calendar_agree() {
        let moment = UtcDateTime::new(2026, 11, 1, 12, 30, 15, 250_000_000).unwrap();
        let ticks = moment.to_ticks();
        assert_eq!(UtcDateTime::from_ticks(ticks), moment);
        assert_eq!(
            ticks_to_unix_ns(ticks),
            1_793_536_215 * 1_000_000_000 + 250_000_000
        );
        assert_eq!(moment.to_unix_ns(), ticks_to_unix_ns(ticks));
        assert_eq!(unix_ns_to_ticks(ticks_to_unix_ns(ticks)), ticks);
        let epoch = UtcDateTime::new(1970, 1, 1, 0, 0, 0, 0).unwrap();
        assert_eq!(epoch.to_ticks(), TICKS_AT_UNIX_EPOCH);
        assert_eq!(
            UtcDateTime::from_ticks(0).to_string(),
            "0001-01-01 00:00:00.000000"
        );
        assert_eq!(
            UtcDateTime::from_unix_ns(1_792_713_600_000_000_000),
            UtcDateTime::from_yyyymmdd(20261023).unwrap()
        );
        assert_eq!(
            UtcDateTime::from_unix_ns(-1).to_string(),
            "1969-12-31 23:59:59.999999"
        );
    }

    #[test]
    fn every_day_of_four_centuries_round_trips() {
        let mut ticks = UtcDateTime::from_yyyymmdd(18_000_101).unwrap().to_ticks();
        let mut previous = UtcDateTime::from_ticks(ticks - 1);
        for _ in 0..146_097 {
            let date = UtcDateTime::from_ticks(ticks);
            assert!(date > previous);
            assert_eq!(UtcDateTime::from_yyyymmdd(date.yyyymmdd()), Some(date));
            assert_eq!(date.to_ticks(), ticks);
            previous = date;
            ticks += TICKS_PER_DAY;
        }
    }

    #[test]
    fn extreme_times_do_not_panic() {
        for ticks in [i64::MIN, i64::MIN + 1, -1, 0, i64::MAX - 1, i64::MAX] {
            let moment = UtcDateTime::from_ticks(ticks);
            assert_eq!(moment.to_ticks(), ticks);
            let _ = (moment.to_unix_ns(), moment.yyyymmdd(), moment.to_string());
            let _ = ticks_to_unix_ns(ticks);
            let _ = unix_ns_to_ticks(ticks);
            let ns = UtcDateTime::from_unix_ns(ticks);
            assert_eq!(ns.to_unix_ns(), ticks);
            let _ = ns.to_ticks();
        }
        let day = UtcDateTime::new(2026, 10, 14, 12, 0, 0, 5).unwrap();
        assert_eq!(
            day.start_of_day(),
            UtcDateTime::from_yyyymmdd(20261014).unwrap()
        );
        assert_eq!(
            (day.year(), day.month(), day.day(), day.hour()),
            (2026, 10, 14, 12)
        );
        assert_eq!((day.minute(), day.second(), day.nanosecond()), (0, 0, 5));
    }

    #[test]
    fn invalid_dates_are_refused() {
        assert!(UtcDateTime::from_yyyymmdd(20260229).is_none());
        assert!(UtcDateTime::from_yyyymmdd(20240229).is_some());
        assert!(UtcDateTime::from_yyyymmdd(21000229).is_none());
        assert!(UtcDateTime::from_yyyymmdd(20261301).is_none());
        assert!(UtcDateTime::from_yyyymmdd(20261100).is_none());
        assert!(UtcDateTime::new(2026, 1, 1, 24, 0, 0, 0).is_none());
        assert!(UtcDateTime::new(0, 1, 1, 0, 0, 0, 0).is_none());
    }

    #[test]
    fn display_precision_sets_the_decimals() {
        let moment = UtcDateTime::new(2026, 10, 14, 0, 29, 59, 123_456_789).unwrap();
        assert_eq!(moment.to_string(), "2026-10-14 00:29:59.123456");
        assert_eq!(format!("{moment:.0}"), "2026-10-14 00:29:59");
        assert_eq!(format!("{moment:.9}"), "2026-10-14 00:29:59.123456789");
        assert_eq!(format!("{moment:.12}"), "2026-10-14 00:29:59.123456789");
    }

    #[test]
    fn fixed_point_numbers() {
        assert_eq!(format_fixed(123_110_000_000), "1231.1");
        assert_eq!(format_fixed(123_100_000_000), "1231");
        assert_eq!(format_fixed(-50_000_000), "-0.5");
        assert_eq!(format_fixed(1), "0.00000001");
        assert_eq!(format_fixed(0), "0");
        assert_eq!(format_fixed(i64::MIN), "-92233720368.54775808");
        assert_eq!(to_f64(12_345_000_000), 123.45);
    }
}
