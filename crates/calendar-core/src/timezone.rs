use chrono::{NaiveDateTime, TimeZone};

pub fn normalize_timezone(tzid: &str) -> Option<chrono_tz::Tz> {
    let clean = tzid.trim_matches('"');
    if let Ok(tz) = clean.parse::<chrono_tz::Tz>() {
        return Some(tz);
    }
    let iana_name = match clean {
        "Turkey Standard Time" => "Europe/Istanbul",
        "W. Europe Standard Time" => "Europe/Berlin",
        "GMT Standard Time" => "Europe/London",
        "Eastern Standard Time" => "America/New_York",
        "Central Standard Time" => "America/Chicago",
        "Mountain Standard Time" => "America/Denver",
        "Pacific Standard Time" => "America/Los_Angeles",
        "UTC" | "Greenwich Standard Time" => "UTC",
        _ => return None,
    };
    iana_name.parse::<chrono_tz::Tz>().ok()
}

pub fn local_system_timezone() -> chrono_tz::Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|tz| tz.parse::<chrono_tz::Tz>().ok())
        .unwrap_or(chrono_tz::UTC)
}

pub fn parse_floating_datetime_to_utc(naive: NaiveDateTime) -> i64 {
    let tz = local_system_timezone();
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.timestamp(),
        chrono::LocalResult::Ambiguous(dt1, _) => dt1.timestamp(),
        chrono::LocalResult::None => naive.and_utc().timestamp(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn parses_valid_iana_tzid() {
        assert_eq!(
            normalize_timezone("Europe/Istanbul"),
            Some(chrono_tz::Europe::Istanbul)
        );
        assert_eq!(
            normalize_timezone("America/New_York"),
            Some(chrono_tz::America::New_York)
        );
    }

    #[test]
    fn maps_windows_tzid_to_iana() {
        assert_eq!(
            normalize_timezone("Turkey Standard Time"),
            Some(chrono_tz::Europe::Istanbul)
        );
        assert_eq!(
            normalize_timezone("W. Europe Standard Time"),
            Some(chrono_tz::Europe::Berlin)
        );
        assert_eq!(
            normalize_timezone("Pacific Standard Time"),
            Some(chrono_tz::America::Los_Angeles)
        );
    }

    #[test]
    fn rejects_unknown_tzid() {
        assert_eq!(normalize_timezone("Nonexistent/Invalid_TZ"), None);
    }

    #[test]
    fn normalizes_floating_time() {
        let naive = NaiveDate::from_ymd_opt(2026, 9, 28)
            .unwrap()
            .and_hms_opt(14, 0, 0)
            .unwrap();
        let epoch = parse_floating_datetime_to_utc(naive);
        assert!(epoch > 0);
    }
}
