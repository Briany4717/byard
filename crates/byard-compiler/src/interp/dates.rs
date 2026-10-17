//! `text.formatDate(pattern)`: an ISO date or date-time, written out.
//!
//! A JSON API sends dates as ISO text (`2026-10-16`, `2026-10-16T14:00`), and
//! an interface shows them as words (`Fri 16 Oct`, `14:00`). The calendar
//! arithmetic that turns one into the other (which weekday a date falls on)
//! is not something an app should write in `byld` with nested ternaries.
//!
//! Patterns, each token replaced and everything else kept:
//!
//! | Token | Example |
//! |---|---|
//! | `YYYY` | `2026` |
//! | `MMMM` | `October` |
//! | `MMM` | `Oct` |
//! | `MM` | `10` |
//! | `M` | `10` (no leading zero) |
//! | `DD` | `06` |
//! | `D` | `6` |
//! | `dddd` | `Friday` |
//! | `ddd` | `Fri` |
//! | `HH` | `14` |
//! | `H` | `9` |
//! | `mm` | `05` |
//!
//! English names only: languages are a separate question, and a date in the
//! wrong language is better than a pattern that silently means different
//! things in different builds.

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// A calendar date and an optional time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct When {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
}

/// Reads `YYYY-MM-DD`, optionally followed by `THH:MM` (seconds and a zone
/// after that are ignored). `None` for anything else, or an impossible date.
fn parse(text: &str) -> Option<When> {
    let bytes = text.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = text.get(range)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = i64::from(number(0..4)?);
    let month = number(5..7)?;
    let day = number(8..10)?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in(year, month) {
        return None;
    }
    let (hour, minute) = if bytes.len() >= 16 && (bytes[10] == b'T' || bytes[10] == b' ') {
        if bytes[13] != b':' {
            return None;
        }
        let (h, m) = (number(11..13)?, number(14..16)?);
        if h > 23 || m > 59 {
            return None;
        }
        (h, m)
    } else if bytes.len() == 10 {
        (0, 0)
    } else {
        return None;
    };
    Some(When {
        year,
        month,
        day,
        hour,
        minute,
    })
}

fn days_in(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

/// Monday is 0. Howard Hinnant's `days_from_civil`, offset so 1970-01-01, a
/// Thursday, is 3.
fn weekday(when: When) -> usize {
    let y = if when.month <= 2 {
        when.year - 1
    } else {
        when.year
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(when.month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(when.day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    usize::try_from((days + 3).rem_euclid(7)).unwrap_or(0)
}

/// `text` written out with `pattern`, or `None` when `text` is not an ISO
/// date or date-time.
#[must_use]
pub fn format_date(text: &str, pattern: &str) -> Option<String> {
    let when = parse(text)?;
    let month = MONTHS[(when.month - 1) as usize];
    let day = WEEKDAYS[weekday(when)];
    // Longest token first, so `MMMM` is not read as `MM` twice.
    let tokens: [(&str, String); 12] = [
        ("YYYY", format!("{:04}", when.year)),
        ("MMMM", month.to_string()),
        ("MMM", month[..3].to_string()),
        ("MM", format!("{:02}", when.month)),
        ("M", when.month.to_string()),
        ("DD", format!("{:02}", when.day)),
        ("D", when.day.to_string()),
        ("dddd", day.to_string()),
        ("ddd", day[..3].to_string()),
        ("HH", format!("{:02}", when.hour)),
        ("H", when.hour.to_string()),
        ("mm", format!("{:02}", when.minute)),
    ];
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut rest = pattern;
    'outer: while !rest.is_empty() {
        for (token, value) in &tokens {
            if let Some(after) = rest.strip_prefix(token) {
                out.push_str(value);
                rest = after;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_written_out() {
        assert_eq!(
            format_date("2026-10-16", "ddd D MMM").as_deref(),
            Some("Fri 16 Oct")
        );
        assert_eq!(
            format_date("2026-10-16", "dddd, D MMMM YYYY").as_deref(),
            Some("Friday, 16 October 2026")
        );
        assert_eq!(format_date("2026-01-05", "DD/MM").as_deref(), Some("05/01"));
        assert_eq!(format_date("2026-01-05", "D/M").as_deref(), Some("5/1"));
    }

    #[test]
    fn a_time_is_written_out() {
        assert_eq!(
            format_date("2026-10-16T14:05", "HH:mm").as_deref(),
            Some("14:05")
        );
        assert_eq!(
            format_date("2026-10-16T09:00", "H h").as_deref(),
            Some("9 h")
        );
        assert_eq!(
            format_date("2026-10-16 23:59:30Z", "HH:mm").as_deref(),
            Some("23:59")
        );
    }

    #[test]
    fn weekdays_are_right_across_centuries_and_leap_days() {
        for (date, day) in [
            ("1970-01-01", "Thu"),
            ("2000-02-29", "Tue"),
            ("2024-02-29", "Thu"),
            ("1900-03-01", "Thu"),
            ("2100-12-31", "Fri"),
            ("1999-12-31", "Fri"),
        ] {
            assert_eq!(format_date(date, "ddd").as_deref(), Some(day), "{date}");
        }
    }

    #[test]
    fn what_is_not_a_date_is_nothing() {
        for text in [
            "",
            "16 Oct",
            "2026-13-01",
            "2026-02-30",
            "2025-02-29",
            "2026-10-16T25:00",
            "2026/10/16",
        ] {
            assert_eq!(format_date(text, "D"), None, "{text}");
        }
    }

    #[test]
    fn letters_that_are_not_tokens_are_kept() {
        assert_eq!(
            format_date("2026-10-16", "The D").as_deref(),
            Some("The 16")
        );
    }
}
