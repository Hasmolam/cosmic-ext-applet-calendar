use regex::Regex;
use std::sync::LazyLock;

static MEETING_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https?://(?:meet\.google\.com|[\w-]+\.zoom\.us|zoom\.us|teams\.microsoft\.com|teams\.live\.com)/[^\s<>]+").expect("invalid regex")
});

pub fn extract_meeting_url(
    url_prop: Option<&str>,
    conference_prop: Option<&str>,
    location_prop: Option<&str>,
    description_prop: Option<&str>,
) -> Option<String> {
    if let Some(url) = url_prop.and_then(clean_and_validate_url) {
        return Some(url);
    }

    if let Some(url) = conference_prop.and_then(clean_and_validate_url) {
        return Some(url);
    }

    if let Some(location) = location_prop {
        let trimmed = location.trim();
        if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && let Some(url) = clean_and_validate_url(trimmed)
        {
            return Some(url);
        }
    }

    if let Some(description) = description_prop
        && let Some(mat) = MEETING_REGEX.find(description)
        && let Some(url) = clean_and_validate_url(mat.as_str())
    {
        return Some(url);
    }

    None
}

fn clean_and_validate_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let cleaned = trimmed.trim_end_matches(['.', ',', ';', ':', ')', ']', '>', '"', '\'']);
    if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
        Some(cleaned.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_explicit_url_first() {
        let url = extract_meeting_url(
            Some("https://custom.link/meeting"),
            Some("https://meet.google.com/abc-defg-hij"),
            None,
            None,
        );
        assert_eq!(url.as_deref(), Some("https://custom.link/meeting"));
    }

    #[test]
    fn extracts_google_conference_property() {
        let url = extract_meeting_url(
            None,
            Some("https://meet.google.com/abc-defg-hij"),
            Some("Room 101"),
            None,
        );
        assert_eq!(url.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
    }

    #[test]
    fn extracts_http_location() {
        let url = extract_meeting_url(
            None,
            None,
            Some("https://company.zoom.us/j/1234567890"),
            None,
        );
        assert_eq!(url.as_deref(), Some("https://company.zoom.us/j/1234567890"));
    }

    #[test]
    fn extracts_from_description_regex_with_punctuation_stripped() {
        let description = "Join our sync at https://teams.microsoft.com/l/meetup-join/19%3ameeting. Please be on time!";
        let url = extract_meeting_url(None, None, Some("Online"), Some(description));
        assert_eq!(
            url.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting")
        );
    }

    #[test]
    fn ignores_non_http_urls() {
        let url = extract_meeting_url(Some("javascript:alert(1)"), None, None, None);
        assert_eq!(url, None);
    }
}
