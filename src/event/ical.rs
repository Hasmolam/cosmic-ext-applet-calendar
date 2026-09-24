// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use crate::event::{CalendarError, CalendarEvent, is_safe_web_url};
use jiff::{
    ToSpan, Zoned,
    civil::{Date, time},
    tz::TimeZone,
};

/// Maximum allowed iCalendar content size (5 MB) to prevent Denial of Service.
const MAX_ICAL_BYTES: usize = 5 * 1024 * 1024;

/// Unfolds folded lines in an iCalendar file according to RFC 5545 Section 3.1.
pub fn unfold_lines(raw: &str) -> Vec<String> {
    let mut unfolded: Vec<String> = Vec::new();
    for line in raw.lines() {
        let trimmed_line = line.trim_end_matches(['\r', '\n']);
        if (trimmed_line.starts_with(' ') || trimmed_line.starts_with('\t')) && !unfolded.is_empty()
        {
            if let Some(last) = unfolded.last_mut() {
                last.push_str(&trimmed_line[1..]);
            }
        } else if !trimmed_line.is_empty() {
            unfolded.push(trimmed_line.to_string());
        }
    }
    unfolded
}

/// Unescapes special characters in text values per RFC 5545 Section 3.3.11.
pub fn unescape_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => result.push('\n'),
                Some('\\') => result.push('\\'),
                Some(';') => result.push(';'),
                Some(',') => result.push(','),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Parses an RFC 5545 date/datetime value into a Jiff `Zoned`.
pub fn parse_ical_datetime(val: &str, is_all_day: bool) -> Result<Zoned, CalendarError> {
    let val = val.trim();
    let tz = TimeZone::system();

    // 1. All-day date (YYYYMMDD)
    if is_all_day || val.len() == 8 {
        if val.len() < 8 {
            return Err(CalendarError::Parse(format!("Invalid date length: {val}")));
        }
        let year: i16 = val[0..4]
            .parse()
            .map_err(|_| CalendarError::Parse(format!("Invalid year: {val}")))?;
        let month: i8 = val[4..6]
            .parse()
            .map_err(|_| CalendarError::Parse(format!("Invalid month: {val}")))?;
        let day: i8 = val[6..8]
            .parse()
            .map_err(|_| CalendarError::Parse(format!("Invalid day: {val}")))?;

        let d = Date::new(year, month, day)
            .map_err(|e| CalendarError::Parse(format!("Invalid date: {e}")))?;
        let dt = d.to_datetime(time(0, 0, 0, 0));
        return dt
            .to_zoned(tz)
            .map_err(|e| CalendarError::Parse(format!("Timezone error: {e}")));
    }

    // 2. UTC Date-Time: YYYYMMDDTHHMMSSZ
    if val.ends_with('Z') && val.contains('T') {
        let clean = &val[..val.len() - 1];
        let parts: Vec<&str> = clean.split('T').collect();
        if parts.len() == 2 && parts[0].len() == 8 && parts[1].len() == 6 {
            let year: i16 = parts[0][0..4]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid year: {val}")))?;
            let month: i8 = parts[0][4..6]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid month: {val}")))?;
            let day: i8 = parts[0][6..8]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid day: {val}")))?;

            let hour: i8 = parts[1][0..2]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid hour: {val}")))?;
            let minute: i8 = parts[1][2..4]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid minute: {val}")))?;
            let second: i8 = parts[1][4..6]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid second: {val}")))?;

            let d = Date::new(year, month, day)
                .map_err(|e| CalendarError::Parse(format!("Invalid date: {e}")))?;
            let t = time(hour, minute, second, 0);
            let dt = d.to_datetime(t);

            // UTC to local zoned time
            return dt
                .to_zoned(TimeZone::UTC)
                .map(|z| z.with_time_zone(tz))
                .map_err(|e| CalendarError::Parse(format!("Zoned conversion error: {e}")));
        }
    }

    // 3. Local Date-Time: YYYYMMDDTHHMMSS
    if val.contains('T') {
        let parts: Vec<&str> = val.split('T').collect();
        if parts.len() == 2 && parts[0].len() == 8 && parts[1].len() >= 6 {
            let year: i16 = parts[0][0..4]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid year: {val}")))?;
            let month: i8 = parts[0][4..6]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid month: {val}")))?;
            let day: i8 = parts[0][6..8]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid day: {val}")))?;

            let hour: i8 = parts[1][0..2]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid hour: {val}")))?;
            let minute: i8 = parts[1][2..4]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid minute: {val}")))?;
            let second: i8 = parts[1][4..6]
                .parse()
                .map_err(|_| CalendarError::Parse(format!("Invalid second: {val}")))?;

            let d = Date::new(year, month, day)
                .map_err(|e| CalendarError::Parse(format!("Invalid date: {e}")))?;
            let t = time(hour, minute, second, 0);
            let dt = d.to_datetime(t);

            return dt
                .to_zoned(tz)
                .map_err(|e| CalendarError::Parse(format!("Zoned conversion error: {e}")));
        }
    }

    Err(CalendarError::Parse(format!(
        "Unsupported iCalendar datetime format: {val}"
    )))
}

/// Searches a text string for Google Meet, Zoom, or Microsoft Teams meeting URLs.
pub fn extract_meeting_url(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let clean = word.trim_matches(['(', ')', '[', ']', '<', '>', ',', ';', '"', '\'', '.']);
        if (clean.starts_with("https://meet.google.com/")
            || clean.starts_with("https://") && clean.contains("zoom.us/j/")
            || clean.starts_with("https://teams.microsoft.com/"))
            && is_safe_web_url(clean)
        {
            return Some(clean.to_string());
        }
    }
    None
}

#[derive(Default)]
struct RawVEvent {
    id: String,
    summary: String,
    description: String,
    location: Option<String>,
    url: Option<String>,
    start_raw: Option<(String, bool)>,
    end_raw: Option<(String, bool)>,
    duration_raw: Option<String>,
    rrule_raw: Option<String>,
    is_cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RruleFreq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone)]
pub struct ParsedRrule {
    pub freq: RruleFreq,
    pub interval: usize,
    pub count: Option<usize>,
    pub until: Option<Zoned>,
    pub by_day: Vec<jiff::civil::Weekday>,
}

pub fn parse_duration(raw: &str) -> Option<jiff::Span> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    if let Ok(span) = s.parse::<jiff::Span>() {
        return Some(span);
    }

    let clean = s.strip_prefix('+').unwrap_or(s);
    let rest = clean.strip_prefix('P')?;

    let mut span = jiff::Span::new();
    let mut in_time = false;
    let mut num_str = String::new();

    for ch in rest.chars() {
        if ch == 'T' {
            in_time = true;
            num_str.clear();
            continue;
        }

        if ch.is_ascii_digit() {
            num_str.push(ch);
        } else {
            let n: i64 = num_str.parse().ok()?;
            num_str.clear();
            match (in_time, ch) {
                (false, 'W') => span = span.checked_add((n * 7).days()).ok()?,
                (false, 'D') => span = span.checked_add(n.days()).ok()?,
                (true, 'H') => span = span.checked_add(n.hours()).ok()?,
                (true, 'M') => span = span.checked_add(n.minutes()).ok()?,
                (true, 'S') => span = span.checked_add(n.seconds()).ok()?,
                _ => return None,
            }
        }
    }

    Some(span)
}

pub fn parse_rrule(raw: &str) -> Option<ParsedRrule> {
    let mut freq = None;
    let mut interval = 1;
    let mut count = None;
    let mut until = None;
    let mut by_day = Vec::new();

    for part in raw.split(';') {
        let (k, v) = match part.split_once('=') {
            Some((k, v)) => (k, v),
            None => continue,
        };
        match k.trim().to_uppercase().as_str() {
            "FREQ" => {
                freq = match v.trim().to_uppercase().as_str() {
                    "DAILY" => Some(RruleFreq::Daily),
                    "WEEKLY" => Some(RruleFreq::Weekly),
                    "MONTHLY" => Some(RruleFreq::Monthly),
                    "YEARLY" => Some(RruleFreq::Yearly),
                    _ => None,
                };
            }
            "INTERVAL" => {
                if let Ok(n) = v.trim().parse::<usize>() {
                    interval = n.max(1);
                }
            }
            "COUNT" => {
                if let Ok(n) = v.trim().parse::<usize>() {
                    count = Some(n);
                }
            }
            "UNTIL" => {
                if let Ok(dt) = parse_ical_datetime(v.trim(), v.trim().len() == 8) {
                    until = Some(dt);
                }
            }
            "BYDAY" => {
                for day_code in v.split(',') {
                    let trimmed = day_code.trim().to_uppercase();
                    let weekday = match trimmed.as_str() {
                        "MO" => Some(jiff::civil::Weekday::Monday),
                        "TU" => Some(jiff::civil::Weekday::Tuesday),
                        "WE" => Some(jiff::civil::Weekday::Wednesday),
                        "TH" => Some(jiff::civil::Weekday::Thursday),
                        "FR" => Some(jiff::civil::Weekday::Friday),
                        "SA" => Some(jiff::civil::Weekday::Saturday),
                        "SU" => Some(jiff::civil::Weekday::Sunday),
                        _ => None,
                    };
                    if let Some(w) = weekday {
                        by_day.push(w);
                    }
                }
            }
            _ => {}
        }
    }

    Some(ParsedRrule {
        freq: freq?,
        interval,
        count,
        until,
        by_day,
    })
}

fn weekday_to_monday_zero(w: jiff::civil::Weekday) -> i64 {
    match w {
        jiff::civil::Weekday::Monday => 0,
        jiff::civil::Weekday::Tuesday => 1,
        jiff::civil::Weekday::Wednesday => 2,
        jiff::civil::Weekday::Thursday => 3,
        jiff::civil::Weekday::Friday => 4,
        jiff::civil::Weekday::Saturday => 5,
        jiff::civil::Weekday::Sunday => 6,
    }
}

pub fn event_overlaps_range(
    event_start_date: Date,
    event_end_date: Date,
    is_all_day: bool,
    range_start: Date,
    range_end: Date,
) -> bool {
    if is_all_day && event_end_date > event_start_date {
        let last_day = event_end_date
            .checked_sub(1.days())
            .unwrap_or(event_start_date);
        event_start_date <= range_end && last_day >= range_start
    } else {
        event_start_date <= range_end && event_end_date >= range_start
    }
}

pub fn expand_rrule(
    base: &CalendarEvent,
    rrule: &ParsedRrule,
    range_start: Date,
    range_end: Date,
) -> Vec<CalendarEvent> {
    let mut occurrences = Vec::new();
    let duration_span = base.start.until(&base.end).unwrap_or_else(|_| 1.hours());

    let mut generated_count = 0;
    const MAX_CYCLES: usize = 500;

    match rrule.freq {
        RruleFreq::Daily => {
            for step in 0..MAX_CYCLES {
                if let Some(limit) = rrule.count
                    && generated_count >= limit
                {
                    break;
                }

                let days_offset = (step * rrule.interval) as i64;
                let occ_start = match base.start.checked_add(days_offset.days()) {
                    Ok(s) => s,
                    Err(_) => break,
                };

                if let Some(ref until) = rrule.until
                    && occ_start > *until
                {
                    break;
                }

                if occ_start.date() > range_end {
                    break;
                }

                let occ_end = occ_start
                    .checked_add(duration_span)
                    .unwrap_or_else(|_| occ_start.clone());

                if event_overlaps_range(
                    occ_start.date(),
                    occ_end.date(),
                    base.is_all_day,
                    range_start,
                    range_end,
                ) {
                    occurrences.push(CalendarEvent {
                        id: format!("{}-occ-{}", base.id, generated_count),
                        summary: base.summary.clone(),
                        start: occ_start,
                        end: occ_end,
                        is_all_day: base.is_all_day,
                        location: base.location.clone(),
                        url: base.url.clone(),
                    });
                }

                generated_count += 1;
            }
        }
        RruleFreq::Weekly => {
            let mut by_days = rrule.by_day.clone();
            if by_days.is_empty() {
                by_days.push(base.start.date().weekday());
            }

            'outer: for week_idx in 0..MAX_CYCLES {
                let week_offset = (week_idx * rrule.interval) as i64;
                let week_ref = match base.start.checked_add(week_offset.weeks()) {
                    Ok(w) => w,
                    Err(_) => break,
                };

                let ref_date = week_ref.date();
                let days_from_monday = weekday_to_monday_zero(ref_date.weekday());
                let monday = match ref_date.checked_sub(days_from_monday.days()) {
                    Ok(m) => m,
                    Err(_) => break,
                };

                for &target_day in &by_days {
                    let target_offset = weekday_to_monday_zero(target_day);
                    let target_date = match monday.checked_add(target_offset.days()) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };

                    let occ_start = match week_ref.with().date(target_date).build() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                    if occ_start < base.start {
                        continue;
                    }

                    if let Some(limit) = rrule.count
                        && generated_count >= limit
                    {
                        break 'outer;
                    }

                    if let Some(ref until) = rrule.until
                        && occ_start > *until
                    {
                        break 'outer;
                    }

                    if occ_start.date() > range_end {
                        break 'outer;
                    }

                    let occ_end = occ_start
                        .checked_add(duration_span)
                        .unwrap_or_else(|_| occ_start.clone());

                    if event_overlaps_range(
                        occ_start.date(),
                        occ_end.date(),
                        base.is_all_day,
                        range_start,
                        range_end,
                    ) {
                        occurrences.push(CalendarEvent {
                            id: format!("{}-occ-{}", base.id, generated_count),
                            summary: base.summary.clone(),
                            start: occ_start,
                            end: occ_end,
                            is_all_day: base.is_all_day,
                            location: base.location.clone(),
                            url: base.url.clone(),
                        });
                    }

                    generated_count += 1;
                }
            }
        }
        RruleFreq::Monthly => {
            for step in 0..MAX_CYCLES {
                if let Some(limit) = rrule.count
                    && generated_count >= limit
                {
                    break;
                }

                let months_offset = (step * rrule.interval) as i64;
                let occ_start = match base.start.checked_add(months_offset.months()) {
                    Ok(s) => s,
                    Err(_) => break,
                };

                if let Some(ref until) = rrule.until
                    && occ_start > *until
                {
                    break;
                }

                if occ_start.date() > range_end {
                    break;
                }

                let occ_end = occ_start
                    .checked_add(duration_span)
                    .unwrap_or_else(|_| occ_start.clone());

                if event_overlaps_range(
                    occ_start.date(),
                    occ_end.date(),
                    base.is_all_day,
                    range_start,
                    range_end,
                ) {
                    occurrences.push(CalendarEvent {
                        id: format!("{}-occ-{}", base.id, generated_count),
                        summary: base.summary.clone(),
                        start: occ_start,
                        end: occ_end,
                        is_all_day: base.is_all_day,
                        location: base.location.clone(),
                        url: base.url.clone(),
                    });
                }

                generated_count += 1;
            }
        }
        RruleFreq::Yearly => {
            for step in 0..MAX_CYCLES {
                if let Some(limit) = rrule.count
                    && generated_count >= limit
                {
                    break;
                }

                let years_offset = (step * rrule.interval) as i64;
                let occ_start = match base.start.checked_add(years_offset.years()) {
                    Ok(s) => s,
                    Err(_) => break,
                };

                if let Some(ref until) = rrule.until
                    && occ_start > *until
                {
                    break;
                }

                if occ_start.date() > range_end {
                    break;
                }

                let occ_end = occ_start
                    .checked_add(duration_span)
                    .unwrap_or_else(|_| occ_start.clone());

                if event_overlaps_range(
                    occ_start.date(),
                    occ_end.date(),
                    base.is_all_day,
                    range_start,
                    range_end,
                ) {
                    occurrences.push(CalendarEvent {
                        id: format!("{}-occ-{}", base.id, generated_count),
                        summary: base.summary.clone(),
                        start: occ_start,
                        end: occ_end,
                        is_all_day: base.is_all_day,
                        location: base.location.clone(),
                        url: base.url.clone(),
                    });
                }

                generated_count += 1;
            }
        }
    }

    occurrences
}

/// Parses an entire iCalendar `.ics` document into a list of `CalendarEvent`s.
/// Filters events to only include those overlapping with `[range_start, range_end]`.
pub fn parse_ical_content(
    content: &str,
    range_start: Date,
    range_end: Date,
) -> Result<Vec<CalendarEvent>, CalendarError> {
    if content.len() > MAX_ICAL_BYTES {
        return Err(CalendarError::Parse(
            "iCalendar content exceeds maximum allowed size".to_string(),
        ));
    }

    let lines = unfold_lines(content);
    let mut events = Vec::new();
    let mut current_event: Option<RawVEvent> = None;

    for line in lines {
        if line == "BEGIN:VEVENT" {
            current_event = Some(RawVEvent::default());
            continue;
        }

        if line == "END:VEVENT" {
            if let Some(raw) = current_event.take() {
                if raw.is_cancelled {
                    continue;
                }

                if let Some((s_val, s_all_day)) = raw.start_raw
                    && let Ok(start) = parse_ical_datetime(&s_val, s_all_day)
                {
                    let is_all_day = s_all_day;
                    let end = if let Some((e_val, e_all_day)) = raw.end_raw {
                        parse_ical_datetime(&e_val, e_all_day).unwrap_or_else(|_| {
                            if is_all_day {
                                start
                                    .checked_add(1.days())
                                    .unwrap_or_else(|_| start.clone())
                            } else {
                                start
                                    .checked_add(1.hours())
                                    .unwrap_or_else(|_| start.clone())
                            }
                        })
                    } else if let Some(dur_str) = raw.duration_raw.as_deref()
                        && let Some(span) = parse_duration(dur_str)
                    {
                        start.checked_add(span).unwrap_or_else(|_| start.clone())
                    } else if is_all_day {
                        start
                            .checked_add(1.days())
                            .unwrap_or_else(|_| start.clone())
                    } else {
                        start
                            .checked_add(1.hours())
                            .unwrap_or_else(|_| start.clone())
                    };

                    let final_url = raw.url.or_else(|| extract_meeting_url(&raw.description));

                    let final_summary = if raw.summary.is_empty() {
                        "(Untitled event)".to_string()
                    } else {
                        raw.summary
                    };

                    let base_id = if raw.id.is_empty() {
                        format!("event-{}", events.len())
                    } else {
                        raw.id
                    };

                    let base_event = CalendarEvent {
                        id: base_id,
                        summary: final_summary,
                        start,
                        end,
                        is_all_day,
                        location: raw.location,
                        url: final_url,
                    };

                    if let Some(rrule_str) = raw.rrule_raw
                        && let Some(parsed_rrule) = parse_rrule(&rrule_str)
                    {
                        let expanded =
                            expand_rrule(&base_event, &parsed_rrule, range_start, range_end);
                        events.extend(expanded);
                    } else if event_overlaps_range(
                        base_event.start.date(),
                        base_event.end.date(),
                        base_event.is_all_day,
                        range_start,
                        range_end,
                    ) {
                        events.push(base_event);
                    }
                }
            }
            continue;
        }

        if let Some(raw) = current_event.as_mut() {
            let (key_part, val_part) = match line.split_once(':') {
                Some((k, v)) => (k, v),
                None => continue,
            };

            let key_upper = key_part.to_uppercase();

            if key_upper == "UID" {
                raw.id = val_part.to_string();
            } else if key_upper == "SUMMARY" {
                raw.summary = unescape_text(val_part);
            } else if key_upper == "DESCRIPTION" {
                raw.description = unescape_text(val_part);
            } else if key_upper == "LOCATION" {
                raw.location = Some(unescape_text(val_part));
            } else if key_upper == "URL" {
                if is_safe_web_url(val_part) {
                    raw.url = Some(val_part.to_string());
                }
            } else if key_upper == "STATUS" {
                if val_part.eq_ignore_ascii_case("CANCELLED") {
                    raw.is_cancelled = true;
                }
            } else if key_upper.starts_with("DTSTART") {
                let is_date = key_upper.contains("VALUE=DATE")
                    || (!val_part.contains('T') && val_part.trim().len() == 8);
                raw.start_raw = Some((val_part.to_string(), is_date));
            } else if key_upper.starts_with("DTEND") {
                let is_date = key_upper.contains("VALUE=DATE")
                    || (!val_part.contains('T') && val_part.trim().len() == 8);
                raw.end_raw = Some((val_part.to_string(), is_date));
            } else if key_upper.starts_with("DURATION") {
                raw.duration_raw = Some(val_part.trim().to_string());
            } else if key_upper.starts_with("RRULE") {
                raw.rrule_raw = Some(val_part.trim().to_string());
            }
        }
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn test_unfold_lines() {
        let raw = "SUMMARY:This is a long\r\n  summary that was\r\n\tfolded\r\nLOCATION:Room 1";
        let unfolded = unfold_lines(raw);
        assert_eq!(unfolded.len(), 2);
        assert_eq!(unfolded[0], "SUMMARY:This is a long summary that wasfolded");
        assert_eq!(unfolded[1], "LOCATION:Room 1");
    }

    #[test]
    fn test_unescape_text() {
        let raw = r"Line 1\nLine 2\, with comma\; and semicolon\\";
        let unescaped = unescape_text(raw);
        assert_eq!(unescaped, "Line 1\nLine 2, with comma; and semicolon\\");
    }

    #[test]
    fn test_parse_simple_vevent() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:event-123\r\n\
SUMMARY:Team Planning\r\n\
DTSTART:20260323T100000Z\r\n\
DTEND:20260323T110000Z\r\n\
DESCRIPTION:Meet link: https://meet.google.com/xyz-123\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 3, 1), date(2026, 3, 31)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "event-123");
        assert_eq!(events[0].summary, "Team Planning");
        assert_eq!(
            events[0].url,
            Some("https://meet.google.com/xyz-123".to_string())
        );
        assert!(!events[0].is_all_day);
    }

    #[test]
    fn test_ignore_cancelled_vevent() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:cancelled-1\r\n\
SUMMARY:Cancelled Meeting\r\n\
STATUS:CANCELLED\r\n\
DTSTART:20260323T100000Z\r\n\
DTEND:20260323T110000Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 3, 1), date(2026, 3, 31)).unwrap();
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_allday_holiday() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:holiday-1\r\n\
SUMMARY:New Year's Day\r\n\
DTSTART;VALUE=DATE:20260101\r\n\
DTEND;VALUE=DATE:20260102\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 1, 1), date(2026, 1, 31)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "New Year's Day");
        assert!(events[0].is_all_day);
        assert_eq!(events[0].start.date(), date(2026, 1, 1));
        assert_eq!(events[0].end.date(), date(2026, 1, 2));
    }

    #[test]
    fn test_multiday_event_exclusive_dtend() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:conference-1\r\n\
SUMMARY:Linux App Summit\r\n\
DTSTART;VALUE=DATE:20260510\r\n\
DTEND;VALUE=DATE:20260513\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 5, 1), date(2026, 5, 31)).unwrap();
        assert_eq!(events.len(), 1);
        let dates = crate::event::covered_dates(&events[0]);
        // May 10, 11, 12 (13 is exclusive)
        assert_eq!(
            dates,
            vec![date(2026, 5, 10), date(2026, 5, 11), date(2026, 5, 12)]
        );
    }

    #[test]
    fn test_meeting_urls_zoom_and_teams() {
        let zoom_text = "Join meeting: https://zoom.us/j/987654321?pwd=abc at 10:00";
        assert_eq!(
            extract_meeting_url(zoom_text),
            Some("https://zoom.us/j/987654321?pwd=abc".to_string())
        );

        let teams_text = "Microsoft Teams: https://teams.microsoft.com/l/meetup-join/123";
        assert_eq!(
            extract_meeting_url(teams_text),
            Some("https://teams.microsoft.com/l/meetup-join/123".to_string())
        );
    }

    #[test]
    fn test_dos_max_bytes_rejection() {
        let huge_content = "A".repeat(6 * 1024 * 1024);
        let res = parse_ical_content(&huge_content, date(2026, 1, 1), date(2026, 1, 31));
        assert!(res.is_err());
    }

    #[test]
    fn test_malformed_ics_handled_gracefully() {
        let bad_ics = "BEGIN:VCALENDAR\nGARBAGE LINE WITHOUT COLON\nDTSTART:INVALID\nEND:VCALENDAR";
        let res = parse_ical_content(bad_ics, date(2026, 1, 1), date(2026, 1, 31)).unwrap();
        assert_eq!(res.len(), 0);
    }

    #[test]
    fn test_meeting_url_trailing_period_stripped() {
        let text = "Please join the call at https://meet.google.com/abc-defg-hij. See you there!";
        assert_eq!(
            extract_meeting_url(text),
            Some("https://meet.google.com/abc-defg-hij".to_string())
        );
    }

    #[test]
    fn test_parse_date_without_explicit_value_date() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:implicit-date-1\r\n\
SUMMARY:National Holiday\r\n\
DTSTART:20260423\r\n\
DTEND:20260424\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 4, 1), date(2026, 4, 30)).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].is_all_day);
        assert_eq!(events[0].start.date(), date(2026, 4, 23));
        assert_eq!(events[0].end.date(), date(2026, 4, 24));
    }

    #[test]
    fn test_parse_duration_pt1h30m() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:duration-test-1\r\n\
SUMMARY:Architecture Design\r\n\
DTSTART:20260615T140000Z\r\n\
DURATION:PT1H30M\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 6, 1), date(2026, 6, 30)).unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        let diff_minutes = ev.start.until((jiff::Unit::Minute, &ev.end)).unwrap();
        assert_eq!(diff_minutes.get_minutes(), 90);
    }

    #[test]
    fn test_rrule_daily_count() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:daily-standup\r\n\
SUMMARY:Sprint Standup\r\n\
DTSTART:20260601T090000Z\r\n\
DURATION:PT15M\r\n\
RRULE:FREQ=DAILY;COUNT=5\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 6, 1), date(2026, 6, 30)).unwrap();
        // Should generate exactly 5 daily occurrences: June 1, 2, 3, 4, 5
        assert_eq!(events.len(), 5);
        for (i, ev) in events.iter().enumerate() {
            assert_eq!(ev.start.date(), date(2026, 6, (1 + i) as i8));
        }
    }

    #[test]
    fn test_rrule_weekly_byday() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:weekly-sync\r\n\
SUMMARY:Team Sync\r\n\
DTSTART:20260601T100000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 6, 1), date(2026, 6, 30)).unwrap();
        // June 1, 2026 is Monday (MO).
        // June 3, 2026 is Wednesday (WE).
        // June 8, 2026 is Monday (MO).
        // June 10, 2026 is Wednesday (WE).
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].start.date(), date(2026, 6, 1));
        assert_eq!(events[1].start.date(), date(2026, 6, 3));
        assert_eq!(events[2].start.date(), date(2026, 6, 8));
        assert_eq!(events[3].start.date(), date(2026, 6, 10));
    }

    #[test]
    fn test_rrule_until() {
        let ics = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:daily-until\r\n\
SUMMARY:Daily Bootcamp\r\n\
DTSTART:20260601T090000Z\r\n\
DURATION:PT1H\r\n\
RRULE:FREQ=DAILY;UNTIL=20260603T235959Z\r\n\
END:VEVENT\r\n\
END:VCALENDAR";

        let events = parse_ical_content(ics, date(2026, 6, 1), date(2026, 6, 30)).unwrap();
        // Should only generate occurrences up to UNTIL boundary: June 1, 2, 3
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].start.date(), date(2026, 6, 1));
        assert_eq!(events[1].start.date(), date(2026, 6, 2));
        assert_eq!(events[2].start.date(), date(2026, 6, 3));
    }
}
