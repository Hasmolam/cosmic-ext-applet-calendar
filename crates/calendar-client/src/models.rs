use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CalendarCollection {
    pub id: String,
    pub account_id: String,
    pub display_name: String,
    pub color: (u8, u8, u8, u8),
    pub is_visible: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CalendarEventSummary {
    pub uid: String,
    pub recurrence_id: String,
    pub collection_id: String,
    pub summary: String,
    pub url: String,
    pub start_time: i64,
    pub end_time: i64,
    pub start_date: String,
    pub end_date: String,
    pub color: (u8, u8, u8, u8),
    pub is_all_day: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct CalendarEventDetail {
    pub uid: String,
    pub recurrence_id: String,
    pub collection_id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub url: String,
    pub start_time: i64,
    pub end_time: i64,
    pub start_date: String,
    pub end_date: String,
    pub is_all_day: bool,
}
