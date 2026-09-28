pub use calendar_client::{CalendarCollection, CalendarEventDetail, CalendarEventSummary};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEventRecord {
    pub collection_id: String,
    pub uid: String,
    pub recurrence_id: String,
    pub recurrence_time: Option<i64>,
    pub href: String,
    pub etag: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub url: Option<String>,
    pub start_time: i64,
    pub end_time: i64,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub duration_days: i64,
    pub is_all_day: bool,
    pub tzid: Option<String>,
    pub rrule: Option<String>,
    pub rrule_until: Option<i64>,
    pub exdate: Option<String>,
    pub raw_ics: String,
    pub status: String,
    pub updated_at: i64,
}
