// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use jiff::civil::Date;

use crate::event::{CalendarError, CalendarEvent, ical::parse_ical_content};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Pluggable trait for fetching calendar events from various backends
/// (e.g. Local ICS files, Evolution Data Server, or future COSMIC Accounts).
pub trait CalendarBackend: Send + Sync {
    fn fetch_events<'a>(
        &'a self,
        start: Date,
        end: Date,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>, CalendarError>>;
}

/// Backend that loads events and public holidays from local `.ics` files
/// without requiring any system daemon (satisfies Issue #1331).
pub struct LocalIcsBackend {
    file_paths: Vec<PathBuf>,
}

impl LocalIcsBackend {
    pub fn new(file_paths: Vec<PathBuf>) -> Self {
        Self { file_paths }
    }

    /// Discovers default calendar locations such as ~/.local/share/calendars/ and /usr/share/calendar/
    pub fn default_locations() -> Self {
        let mut paths = Vec::new();

        if let Ok(env_path) = std::env::var("COSMIC_CALENDAR_ICS") {
            paths.push(PathBuf::from(env_path));
        }

        // System-wide calendars (/usr/share/calendar)
        let sys_cal_dir = PathBuf::from("/usr/share/calendar");
        if sys_cal_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&sys_cal_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("ics") {
                    paths.push(path);
                }
            }
        }

        // User XDG data directory (~/.local/share/calendars and ~/.local/share/cosmic-calendar)
        let base_data_dir = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));

        if let Some(base) = base_data_dir {
            for dir_name in ["calendars", "cosmic-calendar"] {
                let user_cal_dir = base.join(dir_name);
                if user_cal_dir.is_dir()
                    && let Ok(entries) = std::fs::read_dir(&user_cal_dir)
                {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("ics") {
                            paths.push(path);
                        }
                    }
                }
            }
        }

        Self { file_paths: paths }
    }

    pub fn file_paths(&self) -> &[PathBuf] {
        &self.file_paths
    }

    pub fn add_file(&mut self, path: PathBuf) {
        self.file_paths.push(path);
    }
}

impl CalendarBackend for LocalIcsBackend {
    fn fetch_events<'a>(
        &'a self,
        start: Date,
        end: Date,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>, CalendarError>> {
        Box::pin(async move {
            let mut all_events = Vec::new();

            for path in &self.file_paths {
                if path.exists() {
                    match tokio::fs::read_to_string(path).await {
                        Ok(content) => match parse_ical_content(&content, start, end) {
                            Ok(parsed) => {
                                all_events.extend(parsed);
                            }
                            Err(err) => {
                                tracing::warn!(?err, path = ?path, "Skipping corrupted calendar file");
                            }
                        },
                        Err(err) => {
                            tracing::warn!(?err, path = ?path, "Failed to read calendar file");
                        }
                    }
                }
            }

            all_events.sort_by(|a, b| a.start.cmp(&b.start));
            Ok(all_events)
        })
    }
}

/// Composite backend that merges events from multiple underlying backends
/// (e.g. Local ICS + Evolution Data Server).
pub struct CompositeBackend {
    backends: Vec<std::sync::Arc<dyn CalendarBackend>>,
}

impl CompositeBackend {
    pub fn new(backends: Vec<std::sync::Arc<dyn CalendarBackend>>) -> Self {
        Self { backends }
    }
}

impl CalendarBackend for CompositeBackend {
    fn fetch_events<'a>(
        &'a self,
        start: Date,
        end: Date,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>, CalendarError>> {
        Box::pin(async move {
            let mut combined = Vec::new();
            for backend in &self.backends {
                match backend.fetch_events(start, end).await {
                    Ok(events) => combined.extend(events),
                    Err(err) => {
                        tracing::warn!(?err, "Error fetching from backend in composite");
                    }
                }
            }

            // Sort chronologically: start, end, normalized summary, ID
            combined.sort_by(|a, b| {
                a.start
                    .cmp(&b.start)
                    .then_with(|| a.end.cmp(&b.end))
                    .then_with(|| {
                        a.summary
                            .trim()
                            .to_lowercase()
                            .cmp(&b.summary.trim().to_lowercase())
                    })
                    .then_with(|| a.id.cmp(&b.id))
            });

            // Robust multi-backend deduplication:
            // 1. Matches identical non-empty UIDs
            // 2. Matches events occurring at the exact same time with matching summary
            // 3. Merges metadata (URL, location) into the retained event
            let mut deduplicated: Vec<CalendarEvent> = Vec::with_capacity(combined.len());

            for event in combined {
                let mut duplicate_found = false;

                for existing in &mut deduplicated {
                    let id_match =
                        !existing.id.is_empty() && !event.id.is_empty() && existing.id == event.id;

                    let time_and_summary_match = existing.is_all_day == event.is_all_day
                        && existing.start == event.start
                        && existing.end == event.end
                        && existing
                            .summary
                            .trim()
                            .eq_ignore_ascii_case(event.summary.trim());

                    let all_day_holiday_match = existing.is_all_day
                        && event.is_all_day
                        && existing.start.date() == event.start.date()
                        && existing
                            .summary
                            .trim()
                            .eq_ignore_ascii_case(event.summary.trim());

                    if id_match || time_and_summary_match || all_day_holiday_match {
                        duplicate_found = true;
                        if existing.url.is_none() && event.url.is_some() {
                            existing.url = event.url.clone();
                        }
                        if existing.location.is_none() && event.location.is_some() {
                            existing.location = event.location.clone();
                        }
                        if existing.id.is_empty() && !event.id.is_empty() {
                            existing.id = event.id.clone();
                        }
                        break;
                    }
                }

                if !duplicate_found {
                    deduplicated.push(event);
                }
            }

            Ok(deduplicated)
        })
    }
}

/// Mock backend that generates predictable test events.
pub struct MockBackend;

impl CalendarBackend for MockBackend {
    fn fetch_events<'a>(
        &'a self,
        start: Date,
        _end: Date,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>, CalendarError>> {
        Box::pin(async move {
            Ok(crate::event::mock_events_for_month(
                start.year(),
                start.month(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[tokio::test]
    async fn test_mock_backend() {
        let backend = MockBackend;
        let events = backend
            .fetch_events(date(2026, 3, 1), date(2026, 3, 31))
            .await
            .unwrap();
        assert!(!events.is_empty());
    }

    #[tokio::test]
    async fn test_local_ics_backend_with_tempfile() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ics_path = temp_dir.path().join("test_holidays.ics");

        let ics_content = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:national-holiday-1\r\n\
SUMMARY:National Sovereignty Day\r\n\
DTSTART;VALUE=DATE:20260423\r\n\
DTEND;VALUE=DATE:20260424\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        std::fs::write(&ics_path, ics_content).unwrap();

        let backend = LocalIcsBackend::new(vec![ics_path]);
        let events = backend
            .fetch_events(date(2026, 4, 1), date(2026, 4, 30))
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "National Sovereignty Day");
        assert!(events[0].is_all_day);
    }

    #[tokio::test]
    async fn test_composite_backend() {
        let b1 = std::sync::Arc::new(MockBackend);
        let b2 = std::sync::Arc::new(MockBackend);
        let composite = CompositeBackend::new(vec![b1, b2]);

        let events = composite
            .fetch_events(date(2026, 3, 1), date(2026, 3, 31))
            .await
            .unwrap();

        // 3 mock events each with identical IDs ("mock-1", "mock-2", "mock-3")
        // should be deduplicated to 3 distinct events!
        assert_eq!(events.len(), 3);
        // Verify chronological order
        for w in events.windows(2) {
            assert!(w[0].start <= w[1].start);
        }
    }

    #[tokio::test]
    async fn test_local_ics_backend_skips_corrupted_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let broken_path = temp_dir.path().join("broken.ics");
        let valid_path = temp_dir.path().join("valid.ics");

        // Write corrupted file (exceeds MAX_ICAL_BYTES or malformed)
        std::fs::write(&broken_path, "NOT_AN_ICAL_FILE_CORRUPTED").unwrap();

        let valid_ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:valid-evt-1\r\n\
SUMMARY:Valid Standup\r\n\
DTSTART:20260410T090000Z\r\n\
DTEND:20260410T093000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR";
        std::fs::write(&valid_path, valid_ics).unwrap();

        // LocalIcsBackend should skip broken.ics and successfully load valid.ics
        let backend = LocalIcsBackend::new(vec![broken_path, valid_path]);
        let events = backend
            .fetch_events(date(2026, 4, 1), date(2026, 4, 30))
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "Valid Standup");
    }

    #[tokio::test]
    async fn test_composite_backend_same_hour_deduplication() {
        use jiff::civil::time;
        let tz = jiff::tz::TimeZone::UTC;

        let d10 = date(2026, 4, 10).to_zoned(tz.clone()).unwrap();
        let start_10am = d10.with().time(time(10, 0, 0, 0)).build().unwrap();
        let end_11am = d10.with().time(time(11, 0, 0, 0)).build().unwrap();
        let end_1030am = d10.with().time(time(10, 30, 0, 0)).build().unwrap();

        // Three events at the same 10:00 AM hour:
        // - Event 1: Team Standup (id: "standup-1")
        // - Event 2: Dentist (id: "dentist-1")
        // - Event 3: Duplicate of Event 1 (different ID or same, but same hour)
        let ev1 = CalendarEvent {
            id: "standup-1".to_string(),
            summary: "Team Standup".to_string(),
            start: start_10am.clone(),
            end: end_1030am.clone(),
            is_all_day: false,
            location: None,
            url: Some("https://meet.google.com/abc".to_string()),
        };

        let ev2 = CalendarEvent {
            id: "dentist-1".to_string(),
            summary: "Dentist Appointment".to_string(),
            start: start_10am.clone(),
            end: end_11am.clone(),
            is_all_day: false,
            location: Some("Clinic".to_string()),
            url: None,
        };

        let ev1_duplicate = CalendarEvent {
            id: "standup-from-ics".to_string(),   // different ID!
            summary: "team standup ".to_string(), // case & whitespace variation
            start: start_10am.clone(),
            end: end_1030am.clone(),
            is_all_day: false,
            location: Some("Online Room 1".to_string()),
            url: None,
        };

        struct CustomBackend(Vec<CalendarEvent>);
        impl CalendarBackend for CustomBackend {
            fn fetch_events<'a>(
                &'a self,
                _start: Date,
                _end: Date,
            ) -> BoxFuture<'a, Result<Vec<CalendarEvent>, CalendarError>> {
                Box::pin(async move { Ok(self.0.clone()) })
            }
        }

        let b1 = std::sync::Arc::new(CustomBackend(vec![ev1, ev2]));
        let b2 = std::sync::Arc::new(CustomBackend(vec![ev1_duplicate]));
        let composite = CompositeBackend::new(vec![b1, b2]);

        let events = composite
            .fetch_events(date(2026, 4, 1), date(2026, 4, 30))
            .await
            .unwrap();

        // Should deduplicate ev1 and ev1_duplicate, while keeping ev2 (Dentist)!
        assert_eq!(events.len(), 2);
        let summaries: Vec<&str> = events.iter().map(|e| e.summary.as_str()).collect();
        assert!(summaries.contains(&"Team Standup"));
        assert!(summaries.contains(&"Dentist Appointment"));

        // Verify URL from ev1 and location from ev1_duplicate were merged!
        let standup = events.iter().find(|e| e.summary == "Team Standup").unwrap();
        assert_eq!(standup.url, Some("https://meet.google.com/abc".to_string()));
        assert_eq!(standup.location, Some("Online Room 1".to_string()));
    }
}
