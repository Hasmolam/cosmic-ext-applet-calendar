// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

pub mod backend;
pub mod cache;
pub mod eds;
pub mod error;
pub mod ical;

pub use backend::{CalendarBackend, CompositeBackend, LocalIcsBackend, MockBackend};
pub use cache::EventCache;
pub use eds::EdsBackend;
pub use error::CalendarError;

use jiff::{ToSpan, Zoned, civil::Date};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: String,
    pub summary: String,
    pub start: Zoned,
    pub end: Zoned,
    pub is_all_day: bool,
    pub location: Option<String>,
    pub url: Option<String>,
}

/// Validates that a URL is strictly an HTTP or HTTPS web link.
/// Rejects dangerous schemes like javascript:, file:, data:, or shell commands.
pub fn is_safe_web_url(raw_url: &str) -> bool {
    let trimmed = raw_url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return false;
    }

    if let Ok(parsed) = url::Url::parse(trimmed) {
        (parsed.scheme() == "https" || parsed.scheme() == "http") && parsed.host().is_some()
    } else {
        false
    }
}

/// Calculates all dates covered by an event.
/// In RFC 5545, for all-day events (or date-only ranges), DTEND is exclusive: [start, end).
pub fn covered_dates(event: &CalendarEvent) -> Vec<Date> {
    let start_date = event.start.date();
    let mut end_date = event.end.date();

    if end_date < start_date {
        end_date = start_date;
    }

    if event.is_all_day {
        // RFC 5545 exclusive DTEND rule: if DTEND is next day at 00:00,
        // the covered end date is the day before.
        if end_date > start_date
            && let Ok(prev) = end_date.checked_sub(1.days())
        {
            end_date = prev;
        }
    }

    let mut dates = Vec::new();
    let mut curr = start_date;
    while curr <= end_date {
        dates.push(curr);
        if let Ok(next) = curr.checked_add(1.days()) {
            curr = next;
        } else {
            break;
        }
    }
    dates
}

/// Returns a set of all unique dates in the given events slice.
pub fn covered_dates_for_events(events: &[CalendarEvent]) -> HashSet<Date> {
    let mut set = HashSet::new();
    for ev in events {
        for d in covered_dates(ev) {
            set.insert(d);
        }
    }
    set
}

/// Filters and returns all events that are active on the specified date,
/// sorted chronologically (all-day events first, then by start time).
pub fn filter_events_for_date(events: &[CalendarEvent], date: Date) -> Vec<CalendarEvent> {
    let mut filtered: Vec<CalendarEvent> = events
        .iter()
        .filter(|ev| covered_dates(ev).contains(&date))
        .cloned()
        .collect();

    filtered.sort_by(|a, b| match (a.is_all_day, b.is_all_day) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.start.cmp(&b.start),
    });

    filtered
}

/// Generates mock events for the given year and month for prototype and verification.
pub fn mock_events_for_month(year: i16, month: i8) -> Vec<CalendarEvent> {
    use jiff::civil::{date, time};
    let tz = jiff::tz::TimeZone::system();
    let mut events = Vec::new();

    // Event 1: Morning meeting on 5th of the month
    if let Ok(d5) = date(year, month, 5).to_zoned(tz.clone())
        && let (Ok(start), Ok(end)) = (
            d5.with().time(time(10, 0, 0, 0)).build(),
            d5.with().time(time(11, 0, 0, 0)).build(),
        )
    {
        events.push(CalendarEvent {
            id: "mock-1".to_string(),
            summary: "COSMIC Team Sync".to_string(),
            start,
            end,
            is_all_day: false,
            location: Some("Online".to_string()),
            url: Some("https://meet.google.com/abc-defg-hij".to_string()),
        });
    }

    // Event 2: All-day event on 15th (RFC 5545 DTEND is 16th exclusive)
    if let Ok(d15) = date(year, month, 15).to_zoned(tz.clone()) {
        let d16 = date(year, month, 16)
            .to_zoned(tz.clone())
            .unwrap_or_else(|_| d15.clone());
        events.push(CalendarEvent {
            id: "mock-2".to_string(),
            summary: "Pop!_OS Release Planning".to_string(),
            start: d15,
            end: d16,
            is_all_day: true,
            location: None,
            url: None,
        });
    }

    // Event 3: Afternoon meeting on 20th of the month
    if let Ok(d20) = date(year, month, 20).to_zoned(tz)
        && let (Ok(start), Ok(end)) = (
            d20.with().time(time(14, 30, 0, 0)).build(),
            d20.with().time(time(15, 30, 0, 0)).build(),
        )
    {
        events.push(CalendarEvent {
            id: "mock-3".to_string(),
            summary: "Architecture Review".to_string(),
            start,
            end,
            is_all_day: false,
            location: Some("Meeting Room B".to_string()),
            url: Some("https://zoom.us/j/123456789".to_string()),
        });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn test_is_safe_web_url() {
        assert!(is_safe_web_url("https://meet.google.com/abc-defg-hij"));
        assert!(is_safe_web_url("http://localhost:8080/meeting"));

        // Dangerous or invalid schemes must be rejected
        assert!(!is_safe_web_url("javascript:alert(1)"));
        assert!(!is_safe_web_url("file:///etc/passwd"));
        assert!(!is_safe_web_url("data:text/html,<script>alert(1)</script>"));
        assert!(!is_safe_web_url("sh -c 'rm -rf /'"));
        assert!(!is_safe_web_url("not a url"));
        assert!(!is_safe_web_url("https://"));
    }

    #[test]
    fn test_allday_exclusive_dtend() {
        let tz = jiff::tz::TimeZone::UTC;
        let start = date(2026, 3, 5).to_zoned(tz.clone()).unwrap();
        let end = date(2026, 3, 6).to_zoned(tz).unwrap();

        let event = CalendarEvent {
            id: "all-day-test".to_string(),
            summary: "Single Day All-Day".to_string(),
            start,
            end,
            is_all_day: true,
            location: None,
            url: None,
        };

        let dates = covered_dates(&event);
        assert_eq!(dates, vec![date(2026, 3, 5)]);
    }
}
