use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Duration, NaiveDate, TimeZone};
use rrule::RRuleSet;
use rusqlite::{Connection, OptionalExtension};
use rusqlite_migration::{M, Migrations};
use thiserror::Error;

use crate::{
    models::{CalendarCollection, CalendarEventDetail, CalendarEventSummary, StoredEventRecord},
    timezone::local_system_timezone,
};

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        "CREATE TABLE collections (\
        id TEXT PRIMARY KEY, account_id TEXT NOT NULL, href TEXT NOT NULL,\
        display_name TEXT, color TEXT, ctag TEXT, sync_token TEXT,\
        is_visible INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL\
    );\
    CREATE TABLE events (\
        collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,\
        uid TEXT NOT NULL, recurrence_id TEXT NOT NULL DEFAULT '', recurrence_time INTEGER,\
        href TEXT NOT NULL, etag TEXT NOT NULL, summary TEXT, description TEXT, location TEXT, url TEXT,\
        start_time INTEGER NOT NULL, end_time INTEGER NOT NULL, start_date TEXT, end_date TEXT,\
        duration_days INTEGER NOT NULL DEFAULT 0, is_all_day INTEGER NOT NULL DEFAULT 0, tzid TEXT,\
        rrule TEXT, rrule_until INTEGER, exdate TEXT, raw_ics TEXT NOT NULL,\
        status TEXT NOT NULL DEFAULT 'CONFIRMED', updated_at INTEGER NOT NULL,\
        PRIMARY KEY (collection_id, uid, recurrence_id)\
    );\
    CREATE INDEX idx_events_range ON events(start_time, end_time) WHERE is_all_day = 0;\
    CREATE INDEX idx_events_dates ON events(start_date, end_date) WHERE is_all_day = 1;\
    CREATE INDEX idx_events_rrule ON events(start_time, rrule_until) WHERE rrule IS NOT NULL;\
    CREATE INDEX idx_events_recurrence_time ON events(recurrence_time) WHERE recurrence_time IS NOT NULL;\
    CREATE INDEX idx_events_caldav_lookup ON events(collection_id, href);\
    CREATE INDEX idx_events_collection ON events(collection_id);\
    PRAGMA foreign_keys = ON;",
    )])
}

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
}

pub fn data_directory() -> Result<PathBuf, DatabaseError> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "XDG data directory is unavailable",
            )
        })?;
    let directory = base.join("cosmic-calendar");
    fs::create_dir_all(&directory)?;
    set_private_permissions(&directory)?;
    Ok(directory)
}

fn set_private_permissions(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn open_database() -> Result<Connection, DatabaseError> {
    let directory = data_directory()?;
    open_database_in_directory(&directory)
}

pub fn delete_collections_for_account(
    connection: &Connection,
    account_id: &str,
) -> Result<usize, DatabaseError> {
    Ok(connection.execute(
        "DELETE FROM collections WHERE account_id = ?1",
        [account_id],
    )?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredCollectionRecord {
    pub href: String,
    pub display_name: Option<String>,
    pub color: Option<String>,
    pub ctag: Option<String>,
}

impl From<String> for DiscoveredCollectionRecord {
    fn from(href: String) -> Self {
        Self {
            href,
            display_name: None,
            color: None,
            ctag: None,
        }
    }
}

impl From<&str> for DiscoveredCollectionRecord {
    fn from(href: &str) -> Self {
        Self {
            href: href.to_string(),
            display_name: None,
            color: None,
            ctag: None,
        }
    }
}

pub fn upsert_discovered_collections(
    connection: &Connection,
    account_id: &str,
    collections: &[DiscoveredCollectionRecord],
    updated_at: i64,
) -> Result<usize, DatabaseError> {
    let transaction = connection.unchecked_transaction()?;
    for collection in collections {
        let id = format!("{account_id}:{}", collection.href);
        transaction.execute(
            "INSERT INTO collections (id, account_id, href, display_name, color, ctag, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 href = excluded.href,
                 display_name = COALESCE(excluded.display_name, collections.display_name),
                 color = COALESCE(excluded.color, collections.color),
                 ctag = COALESCE(excluded.ctag, collections.ctag),
                 updated_at = excluded.updated_at",
            rusqlite::params![
                id,
                account_id,
                collection.href,
                collection.display_name,
                collection.color,
                collection.ctag,
                updated_at,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(collections.len())
}

pub fn delete_missing_collections(
    connection: &Connection,
    account_id: &str,
    discovered_hrefs: &[String],
) -> Result<usize, DatabaseError> {
    let transaction = connection.unchecked_transaction()?;
    let mut statement =
        transaction.prepare("SELECT id, href FROM collections WHERE account_id = ?1")?;
    let existing = statement.query_map([account_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let stale_ids: Vec<String> = existing
        .filter_map(Result::ok)
        .filter(|(_, href)| !discovered_hrefs.iter().any(|discovered| discovered == href))
        .map(|(id, _)| id)
        .collect();
    drop(statement);
    let mut deleted = 0;
    for id in stale_ids {
        deleted += transaction.execute("DELETE FROM collections WHERE id = ?1", [id])?;
    }
    transaction.commit()?;
    Ok(deleted)
}

pub fn atomic_replace_events(
    connection: &Connection,
    collection_id: &str,
    href: &str,
    events: &[StoredEventRecord],
    updated_at: i64,
) -> Result<(), DatabaseError> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "DELETE FROM events WHERE collection_id = ?1 AND href = ?2",
        rusqlite::params![collection_id, href],
    )?;

    let mut stmt = transaction.prepare_cached(
        "INSERT INTO events (
            collection_id, uid, recurrence_id, recurrence_time, href, etag,
            summary, description, location, url, start_time, end_time,
            start_date, end_date, duration_days, is_all_day, tzid,
            rrule, rrule_until, exdate, raw_ics, status, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10, ?11, ?12,
            ?13, ?14, ?15, ?16, ?17,
            ?18, ?19, ?20, ?21, ?22, ?23
        )",
    )?;

    for ev in events {
        if ev.status.eq_ignore_ascii_case("CANCELLED") {
            continue;
        }
        stmt.execute(rusqlite::params![
            ev.collection_id,
            ev.uid,
            ev.recurrence_id,
            ev.recurrence_time,
            ev.href,
            ev.etag,
            ev.summary,
            ev.description,
            ev.location,
            ev.url,
            ev.start_time,
            ev.end_time,
            ev.start_date,
            ev.end_date,
            ev.duration_days,
            if ev.is_all_day { 1 } else { 0 },
            ev.tzid,
            ev.rrule,
            ev.rrule_until,
            ev.exdate,
            ev.raw_ics,
            ev.status,
            updated_at,
        ])?;
    }
    drop(stmt);

    transaction.commit()?;
    Ok(())
}

pub fn delete_event_file(
    connection: &Connection,
    collection_id: &str,
    href: &str,
) -> Result<usize, DatabaseError> {
    let deleted = connection.execute(
        "DELETE FROM events WHERE collection_id = ?1 AND href = ?2",
        rusqlite::params![collection_id, href],
    )?;
    Ok(deleted)
}

pub fn update_sync_token(
    connection: &Connection,
    collection_id: &str,
    sync_token: &str,
    updated_at: i64,
) -> Result<(), DatabaseError> {
    connection.execute(
        "UPDATE collections SET sync_token = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![sync_token, updated_at, collection_id],
    )?;
    Ok(())
}

pub fn reset_collection_for_full_sync(
    connection: &Connection,
    collection_id: &str,
    updated_at: i64,
) -> Result<(), DatabaseError> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "UPDATE collections SET sync_token = NULL, updated_at = ?1 WHERE id = ?2",
        rusqlite::params![updated_at, collection_id],
    )?;
    transaction.execute(
        "DELETE FROM events WHERE collection_id = ?1",
        rusqlite::params![collection_id],
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn parse_color_tuple(hex_or_rgb: &str) -> (u8, u8, u8, u8) {
    let clean = hex_or_rgb.trim();
    if let Some(hex) = clean.strip_prefix('#') {
        match hex.len() {
            6 => {
                if let (Ok(r), Ok(g), Ok(b)) = (
                    u8::from_str_radix(&hex[0..2], 16),
                    u8::from_str_radix(&hex[2..4], 16),
                    u8::from_str_radix(&hex[4..6], 16),
                ) {
                    return (r, g, b, 255);
                }
            }
            8 => {
                if let (Ok(r), Ok(g), Ok(b), Ok(a)) = (
                    u8::from_str_radix(&hex[0..2], 16),
                    u8::from_str_radix(&hex[2..4], 16),
                    u8::from_str_radix(&hex[4..6], 16),
                    u8::from_str_radix(&hex[6..8], 16),
                ) {
                    return (r, g, b, a);
                }
            }
            _ => {}
        }
    }
    // Fallback default soft blue
    (122, 162, 247, 255)
}

pub fn get_collections(connection: &Connection) -> Result<Vec<CalendarCollection>, DatabaseError> {
    let mut stmt = connection.prepare(
        "SELECT id, account_id, display_name, color, is_visible FROM collections ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let account_id: String = row.get(1)?;
        let display_name: Option<String> = row.get(2)?;
        let color: Option<String> = row.get(3)?;
        let is_visible: i64 = row.get(4)?;

        let parsed_color = color
            .as_deref()
            .map(parse_color_tuple)
            .unwrap_or((122, 162, 247, 255));

        Ok(CalendarCollection {
            id,
            account_id,
            display_name: display_name.unwrap_or_default(),
            color: parsed_color,
            is_visible: is_visible != 0,
        })
    })?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn set_collection_visibility(
    connection: &Connection,
    collection_id: &str,
    visible: bool,
) -> Result<bool, DatabaseError> {
    let rows_affected = connection.execute(
        "UPDATE collections SET is_visible = ?1 WHERE id = ?2",
        rusqlite::params![if visible { 1 } else { 0 }, collection_id],
    )?;
    Ok(rows_affected > 0)
}

pub fn query_events_for_range(
    connection: &Connection,
    start_epoch: i64,
    end_epoch: i64,
) -> Result<Vec<CalendarEventSummary>, DatabaseError> {
    let tz = local_system_timezone();
    let start_date_str = match tz.timestamp_opt(start_epoch, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d").to_string(),
        chrono::LocalResult::Ambiguous(dt, _) => dt.format("%Y-%m-%d").to_string(),
        chrono::LocalResult::None => "1970-01-01".to_string(),
    };
    let end_date_str = match tz.timestamp_opt(end_epoch, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d").to_string(),
        chrono::LocalResult::Ambiguous(dt, _) => dt.format("%Y-%m-%d").to_string(),
        chrono::LocalResult::None => "2099-12-31".to_string(),
    };

    let sql = "SELECT e.collection_id, e.uid, e.recurrence_id, e.recurrence_time, e.summary, \
                      e.url, e.start_time, e.end_time, e.start_date, e.end_date, e.duration_days, \
                      e.is_all_day, e.tzid, e.rrule, e.exdate, c.color \
               FROM events e \
               JOIN collections c ON e.collection_id = c.id \
               WHERE c.is_visible = 1 \
                 AND ( \
                   (e.is_all_day = 0 AND e.rrule IS NULL AND e.recurrence_id = '' \
                    AND e.start_time < :end_epoch AND e.end_time > :start_epoch) \
                   OR \
                   (e.is_all_day = 1 AND e.rrule IS NULL AND e.recurrence_id = '' \
                    AND e.start_date <= :end_date_str AND e.end_date >= :start_date_str) \
                   OR \
                   (e.rrule IS NOT NULL AND e.recurrence_id = '' \
                    AND ((e.is_all_day = 0 AND e.start_time < :end_epoch) \
                         OR (e.is_all_day = 1 AND e.start_date <= :end_date_str)) \
                    AND (e.rrule_until IS NULL OR e.rrule_until > (:start_epoch - 86400))) \
                   OR \
                   (e.recurrence_id != '' \
                    AND ( \
                      (e.is_all_day = 0 AND e.start_time < :end_epoch AND e.end_time > :start_epoch) \
                      OR (e.is_all_day = 1 AND e.start_date <= :end_date_str AND e.end_date >= :start_date_str) \
                      OR (e.recurrence_time >= :start_epoch AND e.recurrence_time < :end_epoch) \
                    )) \
                 )";

    struct RawRow {
        collection_id: String,
        uid: String,
        recurrence_id: String,
        recurrence_time: Option<i64>,
        summary: Option<String>,
        url: Option<String>,
        start_time: i64,
        end_time: i64,
        start_date: Option<String>,
        end_date: Option<String>,
        duration_days: i64,
        is_all_day: bool,
        tzid: Option<String>,
        rrule: Option<String>,
        exdate: Option<String>,
        color: (u8, u8, u8, u8),
    }

    let mut stmt = connection.prepare(sql)?;
    let raw_rows = stmt.query_map(
        rusqlite::named_params! {
            ":start_epoch": start_epoch,
            ":end_epoch": end_epoch,
            ":start_date_str": start_date_str,
            ":end_date_str": end_date_str,
        },
        |row| {
            let color_str: Option<String> = row.get(15)?;
            let parsed_color = color_str
                .as_deref()
                .map(parse_color_tuple)
                .unwrap_or((122, 162, 247, 255));
            let is_all_day_int: i64 = row.get(11)?;

            Ok(RawRow {
                collection_id: row.get(0)?,
                uid: row.get(1)?,
                recurrence_id: row.get(2)?,
                recurrence_time: row.get(3)?,
                summary: row.get(4)?,
                url: row.get(5)?,
                start_time: row.get(6)?,
                end_time: row.get(7)?,
                start_date: row.get(8)?,
                end_date: row.get(9)?,
                duration_days: row.get(10)?,
                is_all_day: is_all_day_int != 0,
                tzid: row.get(12)?,
                rrule: row.get(13)?,
                exdate: row.get(14)?,
                color: parsed_color,
            })
        },
    )?;

    let mut raw_records = Vec::new();
    for row in raw_rows {
        raw_records.push(row?);
    }

    // Partition into singles, masters with RRULE, and exceptions
    let mut singles = Vec::new();
    let mut masters = Vec::new();
    let mut exceptions: HashMap<(String, String), Vec<RawRow>> = HashMap::new(); // (collection_id, uid) -> list of exceptions

    for row in raw_records {
        if !row.recurrence_id.is_empty() {
            exceptions
                .entry((row.collection_id.clone(), row.uid.clone()))
                .or_default()
                .push(row);
        } else if row.rrule.is_some() {
            masters.push(row);
        } else {
            singles.push(row);
        }
    }

    let mut final_events: Vec<CalendarEventSummary> = Vec::new();

    // 1. Add non-recurring events directly
    for s in singles {
        final_events.push(CalendarEventSummary {
            uid: s.uid,
            recurrence_id: s.recurrence_id,
            collection_id: s.collection_id,
            summary: s.summary.unwrap_or_default(),
            url: s.url.unwrap_or_default(),
            start_time: s.start_time,
            end_time: s.end_time,
            start_date: s.start_date.unwrap_or_default(),
            end_date: s.end_date.unwrap_or_default(),
            color: s.color,
            is_all_day: s.is_all_day,
        });
    }

    // 2. Expand RRULE masters in-memory
    let query_start_range = start_epoch - 86400;
    let query_end_range = end_epoch;

    for m in masters {
        let key = (m.collection_id.clone(), m.uid.clone());
        let series_exceptions = exceptions.remove(&key).unwrap_or_default();

        let exdates: Vec<i64> = m
            .exdate
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse::<i64>().ok())
            .collect();

        let rrule_str = m.rrule.as_deref().unwrap_or_default();

        // Calculate dtstart datetime in Utc (or local timezone converted to Utc)
        let dtstart_utc = if m.is_all_day {
            let naive_date = m
                .start_date
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
            naive_date.and_hms_opt(0, 0, 0).unwrap().and_utc()
        } else if let Some(tzid) = m
            .tzid
            .as_deref()
            .and_then(crate::timezone::normalize_timezone)
        {
            match tzid.timestamp_opt(m.start_time, 0) {
                chrono::LocalResult::Single(dt) => dt.to_utc(),
                chrono::LocalResult::Ambiguous(dt, _) => dt.to_utc(),
                chrono::LocalResult::None => {
                    DateTime::from_timestamp(m.start_time, 0).unwrap_or_default()
                }
            }
        } else {
            DateTime::from_timestamp(m.start_time, 0).unwrap_or_default()
        };

        let upper_rrule = rrule_str.to_ascii_uppercase();
        let full_rrule = if upper_rrule.starts_with("RRULE:") {
            format!(
                "DTSTART:{}\n{}",
                dtstart_utc.format("%Y%m%dT%H%M%SZ"),
                upper_rrule
            )
        } else {
            format!(
                "DTSTART:{}\nRRULE:{}",
                dtstart_utc.format("%Y%m%dT%H%M%SZ"),
                upper_rrule
            )
        };

        let instances = if let Ok(rrule_set) = full_rrule.parse::<RRuleSet>() {
            rrule_set.all(500).dates
        } else {
            Vec::new()
        };

        let duration_secs = (m.end_time - m.start_time).max(0);

        for inst in instances {
            let inst_epoch = inst.timestamp();
            if inst_epoch < query_start_range || inst_epoch > query_end_range {
                continue;
            }

            // EXDATE filtering
            if exdates.contains(&inst_epoch) {
                continue;
            }

            // Exception suppression if recurrence_time matches
            if series_exceptions
                .iter()
                .any(|ex| ex.recurrence_time == Some(inst_epoch))
            {
                continue;
            }

            if m.is_all_day {
                let inst_date = inst.date_naive();
                let inst_end_date = inst_date + Duration::days(m.duration_days);
                let inst_start_date_str = inst_date.format("%Y-%m-%d").to_string();
                let inst_end_date_str = inst_end_date.format("%Y-%m-%d").to_string();

                // Check range overlap: start_date <= end_date_str && end_date >= start_date_str
                if inst_start_date_str <= end_date_str && inst_end_date_str >= start_date_str {
                    final_events.push(CalendarEventSummary {
                        uid: m.uid.clone(),
                        recurrence_id: String::new(),
                        collection_id: m.collection_id.clone(),
                        summary: m.summary.clone().unwrap_or_default(),
                        url: m.url.clone().unwrap_or_default(),
                        start_time: 0,
                        end_time: 0,
                        start_date: inst_start_date_str,
                        end_date: inst_end_date_str,
                        color: m.color,
                        is_all_day: true,
                    });
                }
            } else {
                let inst_end_time = inst_epoch + duration_secs;
                if inst_end_time > start_epoch && inst_epoch < end_epoch {
                    final_events.push(CalendarEventSummary {
                        uid: m.uid.clone(),
                        recurrence_id: String::new(),
                        collection_id: m.collection_id.clone(),
                        summary: m.summary.clone().unwrap_or_default(),
                        url: m.url.clone().unwrap_or_default(),
                        start_time: inst_epoch,
                        end_time: inst_end_time,
                        start_date: String::new(),
                        end_date: String::new(),
                        color: m.color,
                        is_all_day: false,
                    });
                }
            }
        }

        // Process exceptions for this master
        for ex in series_exceptions {
            if ex.is_all_day {
                let s_date = ex.start_date.clone().unwrap_or_default();
                let e_date = ex.end_date.clone().unwrap_or_default();
                if s_date <= end_date_str && e_date >= start_date_str {
                    final_events.push(CalendarEventSummary {
                        uid: ex.uid,
                        recurrence_id: ex.recurrence_id,
                        collection_id: ex.collection_id,
                        summary: ex.summary.unwrap_or_default(),
                        url: ex.url.unwrap_or_default(),
                        start_time: 0,
                        end_time: 0,
                        start_date: s_date,
                        end_date: e_date,
                        color: ex.color,
                        is_all_day: true,
                    });
                }
            } else if ex.end_time > start_epoch && ex.start_time < end_epoch {
                final_events.push(CalendarEventSummary {
                    uid: ex.uid,
                    recurrence_id: ex.recurrence_id,
                    collection_id: ex.collection_id,
                    summary: ex.summary.unwrap_or_default(),
                    url: ex.url.unwrap_or_default(),
                    start_time: ex.start_time,
                    end_time: ex.end_time,
                    start_date: String::new(),
                    end_date: String::new(),
                    color: ex.color,
                    is_all_day: false,
                });
            }
        }
    }

    // Process orphan exceptions (where master wasn't in SQL result set)
    for (_, ex_list) in exceptions {
        for ex in ex_list {
            if ex.is_all_day {
                let s_date = ex.start_date.clone().unwrap_or_default();
                let e_date = ex.end_date.clone().unwrap_or_default();
                if s_date <= end_date_str && e_date >= start_date_str {
                    final_events.push(CalendarEventSummary {
                        uid: ex.uid,
                        recurrence_id: ex.recurrence_id,
                        collection_id: ex.collection_id,
                        summary: ex.summary.unwrap_or_default(),
                        url: ex.url.unwrap_or_default(),
                        start_time: 0,
                        end_time: 0,
                        start_date: s_date,
                        end_date: e_date,
                        color: ex.color,
                        is_all_day: true,
                    });
                }
            } else if ex.end_time > start_epoch && ex.start_time < end_epoch {
                final_events.push(CalendarEventSummary {
                    uid: ex.uid,
                    recurrence_id: ex.recurrence_id,
                    collection_id: ex.collection_id,
                    summary: ex.summary.unwrap_or_default(),
                    url: ex.url.unwrap_or_default(),
                    start_time: ex.start_time,
                    end_time: ex.end_time,
                    start_date: String::new(),
                    end_date: String::new(),
                    color: ex.color,
                    is_all_day: false,
                });
            }
        }
    }

    // Sort by start_time ascending (for all-day events, start_date ascending)
    final_events.sort_by(|a, b| {
        if a.is_all_day && b.is_all_day {
            a.start_date.cmp(&b.start_date)
        } else if a.is_all_day {
            std::cmp::Ordering::Less
        } else if b.is_all_day {
            std::cmp::Ordering::Greater
        } else {
            a.start_time.cmp(&b.start_time)
        }
    });

    Ok(final_events)
}

pub fn get_event_detail(
    connection: &Connection,
    collection_id: &str,
    uid: &str,
    recurrence_id: &str,
) -> Result<Option<CalendarEventDetail>, DatabaseError> {
    let mut stmt = connection.prepare(
        "SELECT uid, recurrence_id, collection_id, summary, description, location, url, \
                start_time, end_time, start_date, end_date, is_all_day \
         FROM events \
         WHERE collection_id = ?1 AND uid = ?2 AND recurrence_id = ?3",
    )?;

    let detail = stmt
        .query_row(
            rusqlite::params![collection_id, uid, recurrence_id],
            |row| {
                let is_all_day_int: i64 = row.get(11)?;
                Ok(CalendarEventDetail {
                    uid: row.get(0)?,
                    recurrence_id: row.get(1)?,
                    collection_id: row.get(2)?,
                    summary: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    description: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    location: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    url: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    start_time: row.get(7)?,
                    end_time: row.get(8)?,
                    start_date: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    end_date: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    is_all_day: is_all_day_int != 0,
                })
            },
        )
        .optional()?;

    Ok(detail)
}

fn open_database_in_directory(directory: &Path) -> Result<Connection, DatabaseError> {
    fs::create_dir_all(directory)?;
    set_private_permissions(directory)?;
    let path = directory.join("calendar.db");
    match open_and_migrate(&path) {
        Ok(connection) => Ok(connection),
        Err(error) if is_corruption(&error) => {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let backup = directory.join(format!("calendar.db.corrupted.{timestamp}"));
            fs::rename(&path, backup)?;
            open_and_migrate(&path)
        }
        Err(error) => Err(error),
    }
}

fn open_and_migrate(path: &Path) -> Result<Connection, DatabaseError> {
    let mut connection = Connection::open(path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "busy_timeout", 5000)?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    migrations().to_latest(&mut connection)?;
    Ok(connection)
}

fn is_corruption(error: &DatabaseError) -> bool {
    matches!(error, DatabaseError::Sqlite(rusqlite::Error::SqliteFailure(details, _)) if matches!(details.extended_code, rusqlite::ffi::SQLITE_CORRUPT | rusqlite::ffi::SQLITE_NOTADB))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_calendar_schema() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();
        let count: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('collections', 'events')", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
    }

    #[cfg(unix)]
    #[test]
    fn data_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        set_private_permissions(directory.path()).unwrap();
        assert_eq!(
            std::fs::metadata(directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn corrupt_database_is_archived_and_recreated() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("calendar.db"),
            b"not a sqlite database",
        )
        .unwrap();

        open_database_in_directory(directory.path()).unwrap();

        assert!(directory.path().join("calendar.db").is_file());
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("calendar.db.corrupted."))
        );
    }

    #[test]
    fn stale_collections_are_removed_with_cascade() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, updated_at) VALUES
                     ('keep', 'account-a', 'https://calendar.test/keep', 1),
                     ('stale', 'account-a', 'https://calendar.test/stale', 1)",
                [],
            )
            .unwrap();
        connection
                .execute(
                    "INSERT INTO events (collection_id, uid, href, etag, start_time, end_time, raw_ics, updated_at)
                     VALUES ('stale', 'event-a', '/event-a.ics', 'etag-a', 1, 2, 'BEGIN:VCALENDAR', 1)",
                    [],
                )
                .unwrap();

        let deleted = delete_missing_collections(
            &connection,
            "account-a",
            &["https://calendar.test/keep".to_string()],
        )
        .unwrap();

        assert_eq!(deleted, 1);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM collections", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn removing_account_deletes_its_collections_and_events() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, updated_at) VALUES
                     ('account-a-calendar', 'account-a', 'https://calendar.test/a', 1),
                     ('account-b-calendar', 'account-b', 'https://calendar.test/b', 1)",
                [],
            )
            .unwrap();
        connection
                .execute(
                    "INSERT INTO events (collection_id, uid, href, etag, start_time, end_time, raw_ics, updated_at)
                     VALUES ('account-a-calendar', 'event-a', '/event-a.ics', 'etag-a', 1, 2, 'BEGIN:VCALENDAR', 1)",
                    [],
                )
                .unwrap();

        assert_eq!(
            delete_collections_for_account(&connection, "account-a").unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM collections WHERE account_id = 'account-a'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM collections WHERE account_id = 'account-b'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn discovered_collections_are_upserted() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();
        let collections = vec![
            DiscoveredCollectionRecord {
                href: "https://calendar.test/work".to_string(),
                display_name: Some("Work".to_string()),
                color: Some("#ff0000".to_string()),
                ctag: Some("ctag-1".to_string()),
            },
            DiscoveredCollectionRecord {
                href: "https://calendar.test/personal".to_string(),
                display_name: Some("Personal".to_string()),
                color: None,
                ctag: None,
            },
        ];

        assert_eq!(
            upsert_discovered_collections(&connection, "account-a", &collections, 42).unwrap(),
            2
        );

        // Update single collection with new ctag and updated_at
        let updated = vec![DiscoveredCollectionRecord {
            href: "https://calendar.test/work".to_string(),
            display_name: None,
            color: None,
            ctag: Some("ctag-2".to_string()),
        }];
        assert_eq!(
            upsert_discovered_collections(&connection, "account-a", &updated, 43).unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM collections", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let (display_name, color, ctag, updated_at): (String, String, String, i64) = connection
            .query_row(
                "SELECT display_name, color, ctag, updated_at FROM collections WHERE href = 'https://calendar.test/work'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(display_name, "Work");
        assert_eq!(color, "#ff0000");
        assert_eq!(ctag, "ctag-2");
        assert_eq!(updated_at, 43);
    }

    #[test]
    fn test_secrets_are_never_stored_in_sqlite() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();

        for table in ["collections", "events"] {
            let mut stmt = connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let column_names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .map(|result| result.unwrap().to_lowercase())
                .collect();

            for col in column_names {
                assert!(
                    !col.contains("password")
                        && !col.contains("secret")
                        && !col.contains("credential"),
                    "Table {table} contains prohibited credential column: {col}"
                );
                if col.contains("token") {
                    assert_eq!(
                        col, "sync_token",
                        "Prohibited token column in {table}: {col}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_atomic_replace_events_and_cancellation() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, updated_at) VALUES ('col1', 'acc1', 'https://cal.test/col1', 1)",
                [],
            )
            .unwrap();

        let event1 = StoredEventRecord {
            collection_id: "col1".to_string(),
            uid: "uid-1".to_string(),
            recurrence_id: String::new(),
            recurrence_time: None,
            href: "/col1/ev1.ics".to_string(),
            etag: "etag1".to_string(),
            summary: Some("Event 1".to_string()),
            description: None,
            location: None,
            url: None,
            start_time: 1000,
            end_time: 2000,
            start_date: None,
            end_date: None,
            duration_days: 0,
            is_all_day: false,
            tzid: None,
            rrule: None,
            rrule_until: None,
            exdate: None,
            raw_ics: "BEGIN:VCALENDAR...".to_string(),
            status: "CONFIRMED".to_string(),
            updated_at: 1,
        };

        let event2_cancelled = StoredEventRecord {
            collection_id: "col1".to_string(),
            uid: "uid-2".to_string(),
            recurrence_id: "20261001T100000Z".to_string(),
            recurrence_time: Some(1500),
            href: "/col1/ev1.ics".to_string(),
            etag: "etag1".to_string(),
            summary: Some("Event 2 Cancelled".to_string()),
            description: None,
            location: None,
            url: None,
            start_time: 1500,
            end_time: 2500,
            start_date: None,
            end_date: None,
            duration_days: 0,
            is_all_day: false,
            tzid: None,
            rrule: None,
            rrule_until: None,
            exdate: None,
            raw_ics: "BEGIN:VCALENDAR...".to_string(),
            status: "CANCELLED".to_string(),
            updated_at: 1,
        };

        atomic_replace_events(
            &connection,
            "col1",
            "/col1/ev1.ics",
            &[event1, event2_cancelled],
            10,
        )
        .unwrap();

        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM events WHERE collection_id = 'col1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let summary: String = connection
            .query_row("SELECT summary FROM events WHERE uid = 'uid-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(summary, "Event 1");
    }

    #[test]
    fn test_reset_collection_for_full_sync() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, sync_token, updated_at) VALUES ('col1', 'acc1', 'https://cal.test/col1', 'token-123', 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events (collection_id, uid, href, etag, start_time, end_time, raw_ics, updated_at) \
                 VALUES ('col1', 'e1', '/e1.ics', 'etag1', 10, 20, 'ICS', 1)",
                [],
            )
            .unwrap();

        reset_collection_for_full_sync(&connection, "col1", 100).unwrap();

        let (token, updated_at): (Option<String>, i64) = connection
            .query_row(
                "SELECT sync_token, updated_at FROM collections WHERE id = 'col1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(token, None);
        assert_eq!(updated_at, 100);

        let event_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM events WHERE collection_id = 'col1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 0);
    }

    #[test]
    fn test_query_events_for_range_with_rrule_and_exceptions() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, color, is_visible, updated_at) \
                 VALUES ('col1', 'acc1', 'https://cal.test/col1', '#7aa2f7', 1, 1)",
                [],
            )
            .unwrap();

        // Master weekly event starting on 2026-10-01 10:00:00 UTC (1790848800)
        // Repeat weekly for 3 occurrences:
        // 1. 2026-10-01 10:00 UTC (1790848800)
        // 2. 2026-10-08 10:00 UTC (1791453600) -> rescheduled by exception to 11:00 UTC (1791457200)
        // 3. 2026-10-15 10:00 UTC (1792058400)
        let master = StoredEventRecord {
            collection_id: "col1".to_string(),
            uid: "recur-1".to_string(),
            recurrence_id: String::new(),
            recurrence_time: None,
            href: "/col1/recur.ics".to_string(),
            etag: "etag1".to_string(),
            summary: Some("Weekly Standup".to_string()),
            description: None,
            location: None,
            url: None,
            start_time: 1790848800,
            end_time: 1790852400,
            start_date: None,
            end_date: None,
            duration_days: 0,
            is_all_day: false,
            tzid: Some("UTC".to_string()),
            rrule: Some("FREQ=WEEKLY;COUNT=3".to_string()),
            rrule_until: Some(1792058400),
            exdate: None,
            raw_ics: "ICS".to_string(),
            status: "CONFIRMED".to_string(),
            updated_at: 1,
        };

        // Exception for second occurrence (recurrence_time: 1791453600), moved to 11:00
        let exception = StoredEventRecord {
            collection_id: "col1".to_string(),
            uid: "recur-1".to_string(),
            recurrence_id: "20261008T100000Z".to_string(),
            recurrence_time: Some(1791453600),
            href: "/col1/recur.ics".to_string(),
            etag: "etag1".to_string(),
            summary: Some("Rescheduled Standup".to_string()),
            description: None,
            location: None,
            url: None,
            start_time: 1791457200,
            end_time: 1791460800,
            start_date: None,
            end_date: None,
            duration_days: 0,
            is_all_day: false,
            tzid: Some("UTC".to_string()),
            rrule: None,
            rrule_until: None,
            exdate: None,
            raw_ics: "ICS".to_string(),
            status: "CONFIRMED".to_string(),
            updated_at: 1,
        };

        atomic_replace_events(
            &connection,
            "col1",
            "/col1/recur.ics",
            &[master, exception],
            1,
        )
        .unwrap();

        // Query the range covering all 3 occurrences (October 2026)
        let events = query_events_for_range(&connection, 1790800000, 1792100000).unwrap();
        assert_eq!(events.len(), 3);

        assert_eq!(events[0].summary, "Weekly Standup");
        assert_eq!(events[0].start_time, 1790848800);

        // Occurrence 2 is replaced by the exception with new time and title
        assert_eq!(events[1].summary, "Rescheduled Standup");
        assert_eq!(events[1].start_time, 1791457200);

        assert_eq!(events[2].summary, "Weekly Standup");
        assert_eq!(events[2].start_time, 1792058400);
    }

    #[test]
    fn test_all_day_multi_day_duration_preserved() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, color, is_visible, updated_at) \
                 VALUES ('col1', 'acc1', 'https://cal.test/col1', '#9ece6a', 1, 1)",
                [],
            )
            .unwrap();

        // 3-day all day event recurring daily for 2 occurrences
        let master = StoredEventRecord {
            collection_id: "col1".to_string(),
            uid: "all-day-1".to_string(),
            recurrence_id: String::new(),
            recurrence_time: None,
            href: "/col1/all-day.ics".to_string(),
            etag: "etag1".to_string(),
            summary: Some("Hackathon".to_string()),
            description: None,
            location: None,
            url: None,
            start_time: 0,
            end_time: 0,
            start_date: Some("2026-10-01".to_string()),
            end_date: Some("2026-10-03".to_string()),
            duration_days: 2,
            is_all_day: true,
            tzid: None,
            rrule: Some("FREQ=WEEKLY;COUNT=2".to_string()),
            rrule_until: Some(1791453600),
            exdate: None,
            raw_ics: "ICS".to_string(),
            status: "CONFIRMED".to_string(),
            updated_at: 1,
        };

        atomic_replace_events(&connection, "col1", "/col1/all-day.ics", &[master], 1).unwrap();

        let events = query_events_for_range(&connection, 1790800000, 1792100000).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].start_date, "2026-10-01");
        assert_eq!(events[0].end_date, "2026-10-03");
        assert_eq!(events[1].start_date, "2026-10-08");
        assert_eq!(events[1].end_date, "2026-10-10");
    }

    #[test]
    fn test_get_event_detail_roundtrip() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrations().to_latest(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, updated_at) VALUES ('col1', 'acc1', 'https://cal.test/col1', 1)",
                [],
            )
            .unwrap();

        let event = StoredEventRecord {
            collection_id: "col1".to_string(),
            uid: "ev-detail-1".to_string(),
            recurrence_id: String::new(),
            recurrence_time: None,
            href: "/col1/ev.ics".to_string(),
            etag: "etag1".to_string(),
            summary: Some("Detailed Meeting".to_string()),
            description: Some("Important discussion".to_string()),
            location: Some("Room 101".to_string()),
            url: Some("https://meet.google.com/xyz".to_string()),
            start_time: 1790848800,
            end_time: 1790852400,
            start_date: None,
            end_date: None,
            duration_days: 0,
            is_all_day: false,
            tzid: Some("UTC".to_string()),
            rrule: None,
            rrule_until: None,
            exdate: None,
            raw_ics: "ICS".to_string(),
            status: "CONFIRMED".to_string(),
            updated_at: 1,
        };

        atomic_replace_events(&connection, "col1", "/col1/ev.ics", &[event], 1).unwrap();

        let detail = get_event_detail(&connection, "col1", "ev-detail-1", "")
            .unwrap()
            .expect("Event detail should be found");

        assert_eq!(detail.uid, "ev-detail-1");
        assert_eq!(detail.summary, "Detailed Meeting");
        assert_eq!(detail.description, "Important discussion");
        assert_eq!(detail.location, "Room 101");
        assert_eq!(detail.url, "https://meet.google.com/xyz");
        assert_eq!(detail.start_time, 1790848800);
        assert_eq!(detail.end_time, 1790852400);

        let missing = get_event_detail(&connection, "col1", "nonexistent", "").unwrap();
        assert!(missing.is_none());
    }
}
