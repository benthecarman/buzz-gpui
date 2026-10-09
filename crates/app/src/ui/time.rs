//! Timestamp wording from buzz-app's `src/shared/datetime.ts` (en-US).

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone};

fn local(seconds: u64) -> DateTime<Local> {
    Local
        .timestamp_opt(seconds as i64, 0)
        .single()
        .unwrap_or_else(Local::now)
}

fn days_ago(date: NaiveDate, today: NaiveDate) -> i64 {
    (today - date).num_days()
}

fn clock(time: &DateTime<Local>) -> String {
    time.format("%-I:%M %p").to_string()
}

/// Byline time: "2:34 PM", "Yesterday at 2:34 PM", "Monday at 2:34 PM",
/// "Sat, Jun 20 at 2:34 PM", "Jun 20, 2025 at 2:34 PM".
pub fn byline(seconds: u64) -> String {
    byline_at(seconds, Local::now())
}

fn byline_at(seconds: u64, now: DateTime<Local>) -> String {
    let time = local(seconds);
    let (date, today) = (time.date_naive(), now.date_naive());
    match days_ago(date, today) {
        0 => clock(&time),
        1 => format!("Yesterday at {}", clock(&time)),
        2..=6 => format!("{} at {}", time.format("%A"), clock(&time)),
        _ if date.year() == today.year() => {
            format!("{} at {}", time.format("%a, %b %-d"), clock(&time))
        }
        _ => format!("{} at {}", time.format("%b %-d, %Y"), clock(&time)),
    }
}

/// Continuation gutter: "2:34", without the day period.
pub fn compact(seconds: u64) -> String {
    local(seconds).format("%-I:%M").to_string()
}

/// Day divider: "Today", "Yesterday", "Monday", "Saturday, June 20",
/// "June 20, 2025".
pub fn day(seconds: u64) -> String {
    day_at(seconds, Local::now())
}

fn day_at(seconds: u64, now: DateTime<Local>) -> String {
    let time = local(seconds);
    let (date, today) = (time.date_naive(), now.date_naive());
    match days_ago(date, today) {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => time.format("%A").to_string(),
        _ if date.year() == today.year() => time.format("%A, %B %-d").to_string(),
        _ => time.format("%B %-d, %Y").to_string(),
    }
}

/// Whether two times fall on the same local calendar day.
pub fn same_day(a: u64, b: u64) -> bool {
    local(a).date_naive() == local(b).date_naive()
}

/// Full time for tooltips: "Friday, October 2, 2026 at 3:05:09 PM".
pub fn full(seconds: u64) -> String {
    local(seconds)
        .format("%A, %B %-d, %Y at %-I:%M:%S %p")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap()
    }

    #[test]
    fn bylines_follow_buzz_app() {
        let now = at(2026, 10, 9, 15, 0);
        let s = |t: DateTime<Local>| t.timestamp() as u64;
        assert_eq!(byline_at(s(at(2026, 10, 9, 14, 34)), now), "2:34 PM");
        assert_eq!(
            byline_at(s(at(2026, 10, 8, 9, 5)), now),
            "Yesterday at 9:05 AM"
        );
        assert_eq!(
            byline_at(s(at(2026, 10, 5, 14, 34)), now),
            "Monday at 2:34 PM"
        );
        assert_eq!(
            byline_at(s(at(2026, 6, 20, 14, 34)), now),
            "Sat, Jun 20 at 2:34 PM"
        );
        assert_eq!(
            byline_at(s(at(2025, 6, 20, 14, 34)), now),
            "Jun 20, 2025 at 2:34 PM"
        );
        assert_eq!(day_at(s(at(2026, 6, 20, 1, 0)), now), "Saturday, June 20");
        assert_eq!(day_at(s(at(2025, 6, 20, 1, 0)), now), "June 20, 2025");
    }
}
