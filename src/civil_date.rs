//! Proleptic-Gregorian calendar arithmetic (Howard Hinnant's algorithms), so
//! timestamps can be parsed and formatted without a date library. All times
//! are UTC.

/// Days since 1970-01-01 of a civil date. `m` is 1..=12 and `d` 1..=31;
/// neither is range-checked here.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` of a count of days since 1970-01-01 (negative before).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix seconds of a UTC date and time of day. Fields are not range-checked.
pub fn unix_seconds(y: i64, m: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s
}

/// `YYYY-MM-DD` for Unix seconds, rounding toward the earlier day.
pub fn date_from_unix_seconds(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DD` for Unix milliseconds, rounding toward the earlier day.
pub fn date_from_unix_millis(ms: i64) -> String {
    date_from_unix_seconds(ms.div_euclid(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(date_from_unix_seconds(0), "1970-01-01");
        assert_eq!(date_from_unix_millis(0), "1970-01-01");
    }

    #[test]
    fn leap_days() {
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
    }

    #[test]
    fn a_non_leap_century_has_no_feb_29() {
        let mar1 = days_from_civil(1900, 3, 1);
        assert_eq!(mar1, -25_508);
        assert_eq!(civil_from_days(mar1 - 1), (1900, 2, 28));
        assert_eq!(civil_from_days(mar1), (1900, 3, 1));
    }

    #[test]
    fn year_boundaries() {
        assert_eq!(
            civil_from_days(days_from_civil(1999, 12, 31)),
            (1999, 12, 31)
        );
        assert_eq!(
            days_from_civil(2000, 1, 1) - days_from_civil(1999, 12, 31),
            1
        );
        assert_eq!(
            civil_from_days(days_from_civil(2023, 1, 1) - 1),
            (2022, 12, 31)
        );
    }

    #[test]
    fn negative_days_before_1970() {
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(date_from_unix_seconds(-1), "1969-12-31");
        assert_eq!(date_from_unix_millis(-1), "1969-12-31");
        assert_eq!(date_from_unix_seconds(-86_400), "1969-12-31");
        assert_eq!(date_from_unix_seconds(-86_401), "1969-12-30");
    }

    #[test]
    fn every_day_round_trips_over_four_centuries() {
        for z in -80_000..80_000 {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn unix_seconds_combines_date_and_time() {
        assert_eq!(unix_seconds(2024, 3, 1, 10, 0, 0), 1_709_287_200);
        assert_eq!(unix_seconds(1900, 3, 1, 0, 0, 0), -2_203_891_200);
    }

    #[test]
    fn millis_floor_to_the_earlier_day() {
        assert_eq!(date_from_unix_millis(86_399_999), "1970-01-01");
        assert_eq!(date_from_unix_millis(86_400_000), "1970-01-02");
        assert_eq!(date_from_unix_millis(951_782_400_000), "2000-02-29");
    }
}
