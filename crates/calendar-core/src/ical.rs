use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rrule::RRuleSet;

use crate::{
    link_extractor::extract_meeting_url,
    models::StoredEventRecord,
    timezone::{normalize_timezone, parse_floating_datetime_to_utc},
};

const MAX_DESCRIPTION_BYTES: usize = 64 * 1024; // 64 KB

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

#[derive(Default)]
struct RawVEvent {
    uid: Option<String>,
    recurrence_id: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    url: Option<String>,
    conference_url: Option<String>,
    dtstart_raw: Option<(String, String)>, // (params, value)
    dtend_raw: Option<(String, String)>,
    duration_raw: Option<String>,
    rrule: Option<String>,
    exdates: Vec<(String, String)>,
    status: Option<String>,
}

pub fn parse_ics_events(
    ics_content: &str,
    collection_id: &str,
    href: &str,
    etag: &str,
    updated_at: i64,
) -> Vec<StoredEventRecord> {
    let unfolded = unfold_lines(ics_content);
    let mut raw_events = Vec::new();
    let mut current_event: Option<RawVEvent> = None;

    for line in unfolded {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("BEGIN:VEVENT") {
            current_event = Some(RawVEvent::default());
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(event) = current_event.take() {
                raw_events.push(event);
            }
            continue;
        }

        let Some(event) = current_event.as_mut() else {
            continue;
        };

        let Some((prop_with_params, val)) = trimmed.split_once(':') else {
            continue;
        };
        let (prop, params) = match prop_with_params.split_once(';') {
            Some((p, params)) => (p.trim(), params.trim()),
            None => (prop_with_params.trim(), ""),
        };

        match prop.to_ascii_uppercase().as_str() {
            "UID" => event.uid = Some(val.trim().to_string()),
            "RECURRENCE-ID" => event.recurrence_id = Some(val.trim().to_string()),
            "SUMMARY" => event.summary = Some(unescape_text(val)),
            "DESCRIPTION" => {
                let unescaped = unescape_text(val);
                let capped = if unescaped.len() > MAX_DESCRIPTION_BYTES {
                    format!("{}... [kırpıldı]", &unescaped[..MAX_DESCRIPTION_BYTES])
                } else {
                    unescaped
                };
                event.description = Some(capped);
            }
            "LOCATION" => event.location = Some(unescape_text(val)),
            "URL" => event.url = Some(val.trim().to_string()),
            "X-GOOGLE-CONFERENCE" => event.conference_url = Some(val.trim().to_string()),
            "DTSTART" => event.dtstart_raw = Some((params.to_string(), val.trim().to_string())),
            "DTEND" => event.dtend_raw = Some((params.to_string(), val.trim().to_string())),
            "DURATION" => event.duration_raw = Some(val.trim().to_string()),
            "RRULE" => event.rrule = Some(val.trim().to_string()),
            "EXDATE" => event
                .exdates
                .push((params.to_string(), val.trim().to_string())),
            "STATUS" => event.status = Some(val.trim().to_ascii_uppercase()),
            _ => {}
        }
    }

    let mut records = Vec::new();
    for raw in raw_events {
        let Some(uid) = raw.uid else {
            tracing::warn!(href, "Skipping VEVENT missing UID");
            continue;
        };
        let Some((start_params, start_val)) = raw.dtstart_raw else {
            tracing::warn!(uid = %uid, href, "Skipping VEVENT missing DTSTART");
            continue;
        };

        let is_all_day = start_params.to_ascii_uppercase().contains("VALUE=DATE")
            || (start_val.len() == 8 && !start_val.contains('T'));

        let meeting_url = extract_meeting_url(
            raw.url.as_deref(),
            raw.conference_url.as_deref(),
            raw.location.as_deref(),
            raw.description.as_deref(),
        );

        let recurrence_id = raw.recurrence_id.unwrap_or_default();
        let recurrence_time = if !recurrence_id.is_empty() {
            parse_datetime_or_date(&recurrence_id, "", false).map(|(epoch, _, _)| epoch)
        } else {
            None
        };

        if is_all_day {
            let Some((start_date, start_naive)) = parse_date_only(&start_val) else {
                tracing::warn!(uid = %uid, val = %start_val, "Failed parsing all-day DTSTART");
                continue;
            };

            let (end_date, duration_days) = if let Some((_, end_val)) = raw.dtend_raw {
                if let Some((_, end_naive)) = parse_date_only(&end_val) {
                    if end_naive > start_naive {
                        let inclusive_end = end_naive - Duration::days(1);
                        let days = (inclusive_end - start_naive).num_days().max(0);
                        (inclusive_end.format("%Y-%m-%d").to_string(), days)
                    } else {
                        (start_date.clone(), 0)
                    }
                } else {
                    (start_date.clone(), 0)
                }
            } else {
                (start_date.clone(), 0)
            };

            let rrule_until = raw.rrule.as_deref().and_then(|r| {
                calculate_rrule_until(r, start_naive.and_hms_opt(0, 0, 0).unwrap().and_utc())
            });

            let exdate_str = flatten_exdates(&raw.exdates);

            records.push(StoredEventRecord {
                collection_id: collection_id.to_string(),
                uid,
                recurrence_id,
                recurrence_time,
                href: href.to_string(),
                etag: etag.to_string(),
                summary: raw.summary,
                description: raw.description,
                location: raw.location,
                url: meeting_url,
                start_time: 0,
                end_time: 0,
                start_date: Some(start_date),
                end_date: Some(end_date),
                duration_days,
                is_all_day: true,
                tzid: None,
                rrule: raw.rrule,
                rrule_until,
                exdate: exdate_str,
                raw_ics: ics_content.to_string(),
                status: raw.status.unwrap_or_else(|| "CONFIRMED".to_string()),
                updated_at,
            });
        } else {
            // Timed event
            let Some((start_epoch, tzid, start_utc)) =
                parse_datetime_or_date(&start_val, &start_params, true)
            else {
                tracing::warn!(uid = %uid, val = %start_val, "Failed parsing timed DTSTART");
                continue;
            };

            let end_epoch = if let Some((end_params, end_val)) = raw.dtend_raw {
                parse_datetime_or_date(&end_val, &end_params, true)
                    .map(|(epoch, _, _)| epoch)
                    .unwrap_or(start_epoch)
            } else if let Some(duration_str) = raw.duration_raw {
                start_epoch + parse_iso_duration(&duration_str).unwrap_or(0)
            } else {
                start_epoch
            };

            let rrule_until = raw
                .rrule
                .as_deref()
                .and_then(|r| calculate_rrule_until(r, start_utc));

            let exdate_str = flatten_exdates(&raw.exdates);

            records.push(StoredEventRecord {
                collection_id: collection_id.to_string(),
                uid,
                recurrence_id,
                recurrence_time,
                href: href.to_string(),
                etag: etag.to_string(),
                summary: raw.summary,
                description: raw.description,
                location: raw.location,
                url: meeting_url,
                start_time: start_epoch,
                end_time: end_epoch,
                start_date: None,
                end_date: None,
                duration_days: 0,
                is_all_day: false,
                tzid,
                rrule: raw.rrule,
                rrule_until,
                exdate: exdate_str,
                raw_ics: ics_content.to_string(),
                status: raw.status.unwrap_or_else(|| "CONFIRMED".to_string()),
                updated_at,
            });
        }
    }

    records
}

fn parse_date_only(val: &str) -> Option<(String, NaiveDate)> {
    let clean = val.trim();
    if clean.len() >= 8 {
        let d = NaiveDate::parse_from_str(&clean[..8], "%Y%m%d").ok()?;
        Some((d.format("%Y-%m-%d").to_string(), d))
    } else {
        None
    }
}

fn parse_datetime_or_date(
    val: &str,
    params: &str,
    extract_tzid: bool,
) -> Option<(i64, Option<String>, DateTime<Utc>)> {
    let clean = val.trim();

    // 1. UTC format: YYYYMMDDTHHMMSSZ
    if clean.ends_with('Z') && clean.contains('T') {
        let naive =
            NaiveDateTime::parse_from_str(&clean[..clean.len() - 1], "%Y%m%dT%H%M%S").ok()?;
        let utc = naive.and_utc();
        return Some((utc.timestamp(), Some("UTC".to_string()), utc));
    }

    // 2. Date only
    if clean.len() == 8 && !clean.contains('T') {
        let naive_date = NaiveDate::parse_from_str(clean, "%Y%m%d").ok()?;
        let naive_dt = naive_date.and_hms_opt(0, 0, 0)?;
        let utc = naive_dt.and_utc();
        return Some((utc.timestamp(), None, utc));
    }

    // 3. Local/TZID datetime: YYYYMMDDTHHMMSS
    if let Ok(naive) = NaiveDateTime::parse_from_str(clean, "%Y%m%dT%H%M%S") {
        let tzid_param = if extract_tzid {
            params
                .split(';')
                .find(|p| p.to_ascii_uppercase().starts_with("TZID="))
                .map(|p| p[5..].trim().trim_matches('"'))
        } else {
            None
        };

        if let Some(tz_name) = tzid_param
            && let Some(tz) = normalize_timezone(tz_name)
        {
            let dt = tz
                .from_local_datetime(&naive)
                .single()
                .unwrap_or_else(|| naive.and_utc().with_timezone(&tz));
            let utc = dt.with_timezone(&Utc);
            return Some((utc.timestamp(), Some(tz_name.to_string()), utc));
        }

        // Floating time: use local system timezone
        let epoch = parse_floating_datetime_to_utc(naive);
        let utc = DateTime::from_timestamp(epoch, 0).unwrap_or_default();
        return Some((epoch, None, utc));
    }

    None
}

fn parse_iso_duration(val: &str) -> Option<i64> {
    let clean = val.trim();
    if !clean.starts_with('P') {
        return None;
    }
    let mut total_secs: i64 = 0;
    let mut num_buf = String::new();
    let mut in_time = false;

    for c in clean[1..].chars() {
        if c == 'T' {
            in_time = true;
            continue;
        }
        if c.is_ascii_digit() {
            num_buf.push(c);
        } else {
            let n: i64 = num_buf.parse().ok()?;
            num_buf.clear();
            match c {
                'D' => total_secs += n * 86400,
                'W' => total_secs += n * 7 * 86400,
                'H' if in_time => total_secs += n * 3600,
                'M' if in_time => total_secs += n * 60,
                'S' if in_time => total_secs += n,
                _ => {}
            }
        }
    }

    Some(total_secs)
}

fn flatten_exdates(exdates: &[(String, String)]) -> Option<String> {
    if exdates.is_empty() {
        return None;
    }
    let mut epochs = Vec::new();
    for (params, val) in exdates {
        for part in val.split(',') {
            if let Some((epoch, _, _)) = parse_datetime_or_date(part, params, true) {
                epochs.push(epoch);
            }
        }
    }
    if epochs.is_empty() {
        None
    } else {
        epochs.sort_unstable();
        epochs.dedup();
        Some(
            epochs
                .into_iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

pub fn calculate_rrule_until(rrule_str: &str, dtstart: DateTime<Utc>) -> Option<i64> {
    let upper = rrule_str.to_ascii_uppercase();

    // Check for UNTIL
    if let Some(until_part) = upper.split(';').find(|part| part.starts_with("UNTIL=")) {
        let until_val = &until_part[6..];
        if let Some((epoch, _, _)) = parse_datetime_or_date(until_val, "", false) {
            return Some(epoch);
        }
    }

    // Check for COUNT
    if upper.split(';').any(|part| part.starts_with("COUNT=")) {
        let full_rrule = if upper.starts_with("RRULE:") {
            format!("DTSTART:{}\n{}", dtstart.format("%Y%m%dT%H%M%SZ"), upper)
        } else {
            format!(
                "DTSTART:{}\nRRULE:{}",
                dtstart.format("%Y%m%dT%H%M%SZ"),
                upper
            )
        };

        if let Ok(rrule_set) = full_rrule.parse::<RRuleSet>() {
            let instances = rrule_set.all(1000);
            if let Some(last) = instances.dates.last() {
                return Some(last.timestamp());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_vevent_with_exceptions() {
        let ics = r#"BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Example Corp.//EN
BEGIN:VEVENT
UID:multi-123
SUMMARY:Weekly Sync
DTSTART:20261001T090000Z
DTEND:20261001T100000Z
RRULE:FREQ=WEEKLY;COUNT=3
END:VEVENT
BEGIN:VEVENT
UID:multi-123
RECURRENCE-ID:20261008T090000Z
SUMMARY:Rescheduled Weekly Sync
DTSTART:20261008T110000Z
DTEND:20261008T120000Z
END:VEVENT
END:VCALENDAR"#;

        let records = parse_ics_events(ics, "col1", "/item.ics", "etag1", 100);
        assert_eq!(records.len(), 2);

        let master = records.iter().find(|r| r.recurrence_id.is_empty()).unwrap();
        assert_eq!(master.summary.as_deref(), Some("Weekly Sync"));
        assert!(master.rrule.is_some());
        assert!(master.rrule_until.is_some());

        let exception = records
            .iter()
            .find(|r| !r.recurrence_id.is_empty())
            .unwrap();
        assert_eq!(
            exception.summary.as_deref(),
            Some("Rescheduled Weekly Sync")
        );
        assert_eq!(exception.recurrence_id, "20261008T090000Z");
        assert!(exception.recurrence_time.is_some());
    }

    #[test]
    fn truncates_description_over_64kb() {
        let large_desc = "a".repeat(70000);
        let ics = format!(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:large-desc\nDTSTART:20261001T090000Z\nDESCRIPTION:{large_desc}\nEND:VEVENT\nEND:VCALENDAR"
        );

        let records = parse_ics_events(&ics, "col1", "/large.ics", "etag", 100);
        assert_eq!(records.len(), 1);
        let desc = records[0].description.as_ref().unwrap();
        assert!(desc.ends_with("... [kırpıldı]"));
        assert!(desc.len() <= MAX_DESCRIPTION_BYTES + 30);
    }

    #[test]
    fn inclusive_dtend_for_all_day_events() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
UID:all-day-multiday
DTSTART;VALUE=DATE:20261001
DTEND;VALUE=DATE:20261004
SUMMARY:3 Day Conference
END:VEVENT
END:VCALENDAR"#;

        let records = parse_ics_events(ics, "col1", "/conf.ics", "etag", 100);
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert!(rec.is_all_day);
        assert_eq!(rec.start_date.as_deref(), Some("2026-10-01"));
        assert_eq!(rec.end_date.as_deref(), Some("2026-10-03")); // 4th minus 1 day = 3rd
        assert_eq!(rec.duration_days, 2);
    }

    #[test]
    fn flattens_multi_line_and_comma_exdates() {
        let ics = r#"BEGIN:VCALENDAR
BEGIN:VEVENT
UID:exdate-test
DTSTART:20261001T090000Z
EXDATE:20261008T090000Z,20261015T090000Z
EXDATE:20261022T090000Z
SUMMARY:Exdate Sync
END:VEVENT
END:VCALENDAR"#;

        let records = parse_ics_events(ics, "col1", "/ex.ics", "etag", 100);
        assert_eq!(records.len(), 1);
        let exdate = records[0].exdate.as_ref().unwrap();
        let parts: Vec<&str> = exdate.split(',').collect();
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn calculates_count_rrule_until() {
        let start = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap();
        let until = calculate_rrule_until("FREQ=DAILY;COUNT=5", start);
        assert!(until.is_some());
        let expected = Utc
            .with_ymd_and_hms(2026, 10, 5, 9, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(until.unwrap(), expected);
    }

    #[test]
    fn parses_until_rrule_until() {
        let start = Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap();
        let until = calculate_rrule_until("FREQ=WEEKLY;UNTIL=20261101T000000Z", start);
        assert!(until.is_some());
        let expected = Utc
            .with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(until.unwrap(), expected);
    }
}
