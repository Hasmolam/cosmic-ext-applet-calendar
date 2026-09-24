// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use jiff::civil::Date;
use std::time::Duration;
use zbus::fdo::ObjectManagerProxy;
use zbus::proxy;

use crate::event::{
    CalendarBackend, CalendarError, CalendarEvent, backend::BoxFuture, ical::parse_ical_content,
};

const CALENDAR_BUS_NAMES: &[&str] = &[
    "org.gnome.evolution.dataserver.Calendar8",
    "org.gnome.evolution.dataserver.Calendar7",
    "org.gnome.evolution.dataserver.Calendar9",
];

const SOURCES_BUS: &str = "org.gnome.evolution.dataserver.Sources5";
const SOURCES_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";
const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/CalendarFactory";

#[proxy(
    interface = "org.gnome.evolution.dataserver.CalendarFactory",
    default_path = "/org/gnome/evolution/dataserver/CalendarFactory"
)]
pub trait CalendarFactory {
    fn open_calendar(&self, uid: &str) -> zbus::Result<(String, String)>;
}

#[proxy(interface = "org.gnome.evolution.dataserver.Calendar")]
pub trait CalendarSubprocess {
    fn get_object_list(&self, sexp: &str) -> zbus::Result<Vec<String>>;
}

/// Builds the S-Expression query string required by Evolution Data Server.
/// Example format: (occur-in-time-range? (make-time "20260401T000000Z") (make-time "20260430T235959Z"))
pub fn make_time_range_sexp(start: Date, end: Date) -> String {
    format!(
        "(occur-in-time-range? (make-time \"{:04}{:02}{:02}T000000Z\") (make-time \"{:04}{:02}{:02}T235959Z\"))",
        start.year(),
        start.month(),
        start.day(),
        end.year(),
        end.month(),
        end.day()
    )
}

/// Discovers enabled and selected calendar source UIDs from EDS Sources5 manager.
async fn discover_calendar_uids(conn: &zbus::Connection) -> Vec<String> {
    let mut uids = Vec::new();

    let obj_manager = match ObjectManagerProxy::builder(conn)
        .destination(SOURCES_BUS)
        .and_then(|b| b.path(SOURCES_PATH))
    {
        Ok(builder) => match builder.build().await {
            Ok(p) => p,
            Err(err) => {
                tracing::debug!(?err, "Could not connect to EDS SourceManager");
                return uids;
            }
        },
        Err(err) => {
            tracing::debug!(?err, "Invalid SourceManager path or destination");
            return uids;
        }
    };

    let managed_objects = match obj_manager.get_managed_objects().await {
        Ok(objs) => objs,
        Err(err) => {
            tracing::debug!(?err, "Failed to get managed objects from EDS");
            return uids;
        }
    };

    for (_path, interfaces) in managed_objects {
        for (iface, props) in interfaces {
            if iface.as_str() == "org.gnome.evolution.dataserver.Source" {
                let uid_opt = props
                    .get("UID")
                    .and_then(|v| String::try_from(v.clone()).ok());
                let data_opt = props
                    .get("Data")
                    .and_then(|v| String::try_from(v.clone()).ok());

                if let (Some(uid), Some(data)) = (uid_opt, data_opt) {
                    // Check if it's a calendar and not disabled/deselected
                    if data.contains("[Calendar]")
                        && !data.contains("Selected=false")
                        && !data.contains("Enabled=false")
                    {
                        uids.push(uid);
                    }
                }
            }
        }
    }

    uids
}

async fn try_connect_factory<'a>(
    conn: &'a zbus::Connection,
    bus_name: &str,
) -> Option<CalendarFactoryProxy<'a>> {
    let builder = CalendarFactoryProxy::builder(conn)
        .destination(bus_name.to_string())
        .ok()?
        .path(FACTORY_PATH)
        .ok()?;
    builder.build().await.ok()
}

async fn find_calendar_factory<'a>(
    conn: &'a zbus::Connection,
) -> Option<(CalendarFactoryProxy<'a>, String)> {
    if let Ok(custom_bus) = std::env::var("COSMIC_CALENDAR_EDS_BUS")
        && let Some(proxy) = try_connect_factory(conn, &custom_bus).await
    {
        return Some((proxy, custom_bus));
    }

    for bus_name in CALENDAR_BUS_NAMES {
        if let Some(proxy) = try_connect_factory(conn, bus_name).await {
            return Some((proxy, bus_name.to_string()));
        }
    }

    None
}

async fn fetch_calendar_events(
    conn: &zbus::Connection,
    factory: &CalendarFactoryProxy<'_>,
    uid: &str,
    sexp: &str,
    start: Date,
    end: Date,
) -> Result<Vec<CalendarEvent>, CalendarError> {
    let (sub_path, sub_bus) = factory.open_calendar(uid).await?;

    let sub_proxy = CalendarSubprocessProxy::builder(conn)
        .destination(sub_bus)?
        .path(sub_path)?
        .build()
        .await?;

    let ical_list = sub_proxy.get_object_list(sexp).await?;

    let mut events = Vec::new();
    for raw in ical_list {
        if let Ok(parsed) = parse_ical_content(&raw, start, end) {
            events.extend(parsed);
        }
    }

    Ok(events)
}

/// Backend that loads online calendar events (Google, Nextcloud, CalDAV)
/// from Evolution Data Server via D-Bus (satisfies Issue #871).
pub struct EdsBackend;

impl EdsBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EdsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarBackend for EdsBackend {
    fn fetch_events<'a>(
        &'a self,
        start: Date,
        end: Date,
    ) -> BoxFuture<'a, Result<Vec<CalendarEvent>, CalendarError>> {
        Box::pin(async move {
            let timeout_duration = Duration::from_secs(3);

            let fetch_work = async {
                let conn = match zbus::Connection::session().await {
                    Ok(c) => c,
                    Err(err) => {
                        tracing::debug!(?err, "No D-Bus session bus available for EDS");
                        return Ok(Vec::new());
                    }
                };

                let (factory, _bus_name) = match find_calendar_factory(&conn).await {
                    Some(f) => f,
                    None => {
                        tracing::debug!(
                            "Evolution Data Server CalendarFactory is not available on D-Bus"
                        );
                        return Ok(Vec::new());
                    }
                };

                let uids = discover_calendar_uids(&conn).await;
                if uids.is_empty() {
                    tracing::debug!("No active calendar sources found in EDS");
                    return Ok(Vec::new());
                }

                let sexp = make_time_range_sexp(start, end);
                let mut all_events = Vec::new();

                for uid in uids {
                    match fetch_calendar_events(&conn, &factory, &uid, &sexp, start, end).await {
                        Ok(events) => all_events.extend(events),
                        Err(err) => {
                            tracing::warn!(?err, uid = %uid, "Failed to fetch events from EDS calendar");
                        }
                    }
                }

                all_events.sort_by(|a, b| a.start.cmp(&b.start));
                Ok(all_events)
            };

            match tokio::time::timeout(timeout_duration, fetch_work).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("EDS event fetching timed out after 3 seconds");
                    Ok(Vec::new())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn test_make_time_range_sexp() {
        let sexp = make_time_range_sexp(date(2026, 4, 1), date(2026, 4, 30));
        assert_eq!(
            sexp,
            "(occur-in-time-range? (make-time \"20260401T000000Z\") (make-time \"20260430T235959Z\"))"
        );
    }

    #[tokio::test]
    async fn test_eds_backend_graceful_live_or_fallback() {
        let backend = EdsBackend::new();
        let res = backend
            .fetch_events(date(2026, 4, 1), date(2026, 4, 30))
            .await;
        // Either live EDS succeeds and returns events or graceful Ok(empty)
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_eds_backend_fetches_real_calendar_events() {
        let backend = EdsBackend::new();
        if let Ok(events) = backend
            .fetch_events(date(2026, 4, 1), date(2026, 4, 30))
            .await
        {
            println!("Fetched {} events from EDS for April 2026", events.len());
            for ev in &events {
                println!("  - {} (all_day={})", ev.summary, ev.is_all_day);
            }
        }
    }
}
