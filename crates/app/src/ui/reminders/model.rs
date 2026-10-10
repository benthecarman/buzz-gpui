//! Due times, groups and time choices (`bundled/reminders/model.ts`).

use buzz_relay_client::{Reminder, ReminderStatus};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use std::collections::HashSet;

pub fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn count_due(reminders: &[Reminder], now: u64) -> usize {
    reminders.iter().filter(|r| r.is_due(now)).count()
}

/// Pending reminders that came due in `(watermark, now]`. The strict
/// lower bound keeps reminders already overdue at launch from notifying
/// again.
pub fn due_since(reminders: &[Reminder], watermark: u64, now: u64) -> Vec<&Reminder> {
    reminders
        .iter()
        .filter(|r| r.is_due(now) && r.not_before.unwrap_or(0) > watermark)
        .collect()
}

/// Timers in buzz-app fire at once past 2^31-1 ms, so long delays are
/// re-checked.
pub const MAX_DELAY_MS: u64 = (1 << 31) - 1;

/// Milliseconds until the next pending reminder comes due, capped.
pub fn next_delay(reminders: &[Reminder], now: u64) -> Option<u64> {
    reminders
        .iter()
        .filter(|r| r.status == ReminderStatus::Pending)
        .filter_map(|r| r.not_before.filter(|at| *at > now))
        .min()
        .map(|next| ((next - now) * 1000).min(MAX_DELAY_MS))
}

/// Message ids with a pending reminder.
pub fn pending_message_ids(reminders: &[Reminder]) -> HashSet<String> {
    reminders
        .iter()
        .filter(|r| r.status == ReminderStatus::Pending)
        .filter_map(|r| r.target.as_ref().map(|t| t.event_id.clone()))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub label: &'static str,
    pub reminders: Vec<Reminder>,
}

/// Overdue, Today and Upcoming pending reminders, then Done newest
/// first; cancelled ones are hidden.
pub fn group_reminders(reminders: &[Reminder], now: u64, end_of_today: u64) -> Vec<Group> {
    let mut groups: [Group; 4] = ["Overdue", "Today", "Upcoming", "Done"].map(|label| Group {
        label,
        reminders: Vec::new(),
    });
    for reminder in reminders {
        if reminder.status == ReminderStatus::Done {
            groups[3].reminders.push(reminder.clone());
        }
        let Some(at) = reminder.not_before else {
            continue;
        };
        if reminder.status != ReminderStatus::Pending {
            continue;
        }
        let index = if at <= now {
            0
        } else if at <= end_of_today {
            1
        } else {
            2
        };
        groups[index].reminders.push(reminder.clone());
    }
    groups
        .into_iter()
        .filter(|group| !group.reminders.is_empty())
        .map(|mut group| {
            if group.label == "Done" {
                group
                    .reminders
                    .sort_by_key(|r| std::cmp::Reverse(r.created_at));
            } else {
                group.reminders.sort_by_key(|r| r.not_before.unwrap_or(0));
            }
            group
        })
        .collect()
}

/// The last second of today, local time.
pub fn end_of_today() -> u64 {
    let today = Local::now().date_naive();
    local_seconds(today.and_hms_opt(23, 59, 59).expect("a valid time")).unwrap_or(0)
}

/// A local date and time in unix seconds, as JavaScript's `Date` reads
/// it: a time skipped by daylight saving moves forward, an ambiguous one
/// takes the earlier instant.
fn local_seconds(at: NaiveDateTime) -> Option<u64> {
    let resolved = Local.from_local_datetime(&at).earliest().or_else(|| {
        Local
            .from_local_datetime(&(at + Duration::hours(1)))
            .earliest()
    })?;
    u64::try_from(resolved.timestamp()).ok()
}

/// The day `offset` days from today at 9am, a day later if already past.
fn next_day_at_9am(offset: i64) -> u64 {
    let now = Local::now();
    let nine = NaiveTime::from_hms_opt(9, 0, 0).expect("a valid time");
    let mut day = now.date_naive() + Duration::days(offset);
    let mut at = local_seconds(day.and_time(nine)).unwrap_or(0);
    if at <= now.timestamp() as u64 {
        day += Duration::days(1);
        at = local_seconds(day.and_time(nine)).unwrap_or(0);
    }
    at
}

#[derive(Clone, Copy)]
pub struct TimePreset {
    pub label: &'static str,
    pub at: fn() -> u64,
}

pub const TIME_PRESETS: [TimePreset; 5] = [
    TimePreset {
        label: "In 30 minutes",
        at: || now_seconds() + 30 * 60,
    },
    TimePreset {
        label: "In 1 hour",
        at: || now_seconds() + 60 * 60,
    },
    TimePreset {
        label: "In 3 hours",
        at: || now_seconds() + 3 * 60 * 60,
    },
    TimePreset {
        label: "Tomorrow at 9am",
        at: || next_day_at_9am(1),
    },
    TimePreset {
        label: "Next Monday at 9am",
        at: || {
            // Days until the next Monday, a week out on a Monday.
            let weekday = i64::from(Local::now().weekday().num_days_from_sunday());
            let offset = (8 - weekday) % 7;
            next_day_at_9am(if offset == 0 { 7 } else { offset })
        },
    },
];

/// Today as local `YYYY-MM-DD`, for the custom date field.
pub fn today_date_string() -> String {
    Local::now().date_naive().format("%Y-%m-%d").to_string()
}

/// A strictly future local date and time in unix seconds.
pub fn parse_custom_date_time(date: &str, time: &str) -> Option<u64> {
    let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let time = NaiveTime::parse_from_str(time.trim(), "%H:%M").ok()?;
    local_seconds(date.and_time(time)).filter(|at| *at > now_seconds())
}

/// True when daylight saving skipped the entered local time and `at`
/// moved.
pub fn shifted_from(time: &str, at: u64) -> bool {
    let Some(local) = Local.timestamp_opt(at as i64, 0).single() else {
        return false;
    };
    local.format("%H:%M").to_string() != time.trim().get(..5).unwrap_or(time.trim())
}

/// `toLocaleString(dateStyle: "medium", timeStyle: "short")` in en-US:
/// "Oct 10, 2026, 9:00 AM".
pub fn format_due(at: u64) -> String {
    match Local.timestamp_opt(at as i64, 0).single() {
        Some(local) => local.format("%b %-d, %Y, %-I:%M %p").to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn reminder(
        id: &str,
        status: ReminderStatus,
        not_before: Option<u64>,
        created: u64,
    ) -> Reminder {
        Reminder {
            id: id.into(),
            event_id: id.into(),
            created_at: created,
            not_before,
            status,
            target: None,
            note: Some("n".into()),
        }
    }

    #[test]
    fn groups_split_by_due_time_and_hide_cancelled() {
        let list = [
            reminder("late", ReminderStatus::Pending, Some(50), 1),
            reminder("soon", ReminderStatus::Pending, Some(150), 1),
            reminder("later", ReminderStatus::Pending, Some(500), 1),
            reminder("old", ReminderStatus::Done, None, 1),
            reminder("new", ReminderStatus::Done, None, 9),
            reminder("gone", ReminderStatus::Cancelled, None, 1),
        ];
        let groups = group_reminders(&list, 100, 200);
        let labels: Vec<_> = groups.iter().map(|g| g.label).collect();
        assert_eq!(labels, ["Overdue", "Today", "Upcoming", "Done"]);
        let done: Vec<_> = groups[3].reminders.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(done, ["new", "old"]);
    }

    #[test]
    fn due_counts_and_delays() {
        let list = [
            reminder("a", ReminderStatus::Pending, Some(90), 1),
            reminder("b", ReminderStatus::Pending, Some(110), 1),
            reminder("c", ReminderStatus::Done, Some(10), 1),
        ];
        assert_eq!(count_due(&list, 100), 1);
        assert_eq!(
            due_since(&list, 90, 100).len(),
            0,
            "the baseline is exclusive"
        );
        assert_eq!(due_since(&list, 89, 100).len(), 1);
        assert_eq!(next_delay(&list, 100), Some(10_000));
        let far = [reminder(
            "f",
            ReminderStatus::Pending,
            Some(100 + (1 << 40)),
            1,
        )];
        assert_eq!(next_delay(&far, 100), Some(MAX_DELAY_MS));
    }

    #[test]
    fn custom_times_must_be_in_the_future() {
        assert_eq!(parse_custom_date_time("2000-01-01", "09:00"), None);
        assert_eq!(parse_custom_date_time("2999-01-01", ""), None);
        assert!(parse_custom_date_time("2999-01-01", "09:00").is_some());
        assert!(!format_due(0).is_empty());
        for preset in TIME_PRESETS {
            assert!((preset.at)() > now_seconds(), "{}", preset.label);
        }
    }
}
