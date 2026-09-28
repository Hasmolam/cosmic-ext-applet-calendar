use std::sync::Arc;
use std::sync::Mutex;
use zbus::{Connection, Result, proxy};

use crate::models::{CalendarCollection, CalendarEventDetail, CalendarEventSummary};

type EventsCache = Option<(i64, i64, Vec<CalendarEventSummary>)>;

#[proxy(
    interface = "dev.cosmic.Calendar",
    default_service = "dev.cosmic.Calendar",
    default_path = "/dev/cosmic/Calendar"
)]
trait CalendarDbus {
    async fn get_collections(&self) -> Result<Vec<CalendarCollection>>;
    async fn set_collection_visibility(&self, id: &str, visible: bool) -> Result<()>;
    async fn get_events(&self, start_time: i64, end_time: i64)
    -> Result<Vec<CalendarEventSummary>>;
    async fn get_event_details(
        &self,
        collection_id: &str,
        uid: &str,
        recurrence_id: &str,
    ) -> Result<CalendarEventDetail>;
    async fn sync(&self) -> Result<()>;

    #[zbus(signal)]
    fn sync_status_changed(&self, is_syncing: bool) -> Result<()>;

    #[zbus(signal)]
    fn events_changed(&self) -> Result<()>;

    #[zbus(signal)]
    fn collections_changed(&self) -> Result<()>;
}

#[derive(Clone)]
pub struct CalendarClient {
    connection: Connection,
    events_cache: Arc<Mutex<EventsCache>>,
}

impl CalendarClient {
    pub async fn session() -> Result<Self> {
        Ok(Self {
            connection: Connection::session().await?,
            events_cache: Arc::new(Mutex::new(None)),
        })
    }

    async fn proxy(&self) -> Result<CalendarDbusProxy<'_>> {
        CalendarDbusProxy::new(&self.connection).await
    }

    pub async fn get_collections(&self) -> Vec<CalendarCollection> {
        match self.proxy().await {
            Ok(proxy) => proxy.get_collections().await.unwrap_or_else(|error| {
                eprintln!("Calendar daemon is unavailable: {error}");
                Vec::new()
            }),
            Err(error) => {
                eprintln!("Calendar daemon is unavailable: {error}");
                Vec::new()
            }
        }
    }

    pub async fn get_events(&self, start_time: i64, end_time: i64) -> Vec<CalendarEventSummary> {
        {
            let cache = self
                .events_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((cached_start, cached_end, events)) = cache.as_ref()
                && *cached_start == start_time
                && *cached_end == end_time
            {
                return events.clone();
            }
        }

        let events = match self.proxy().await {
            Ok(proxy) => proxy
                .get_events(start_time, end_time)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("Calendar daemon is unavailable: {error}");
                    Vec::new()
                }),
            Err(error) => {
                eprintln!("Calendar daemon is unavailable: {error}");
                Vec::new()
            }
        };
        *self
            .events_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((start_time, end_time, events.clone()));
        events
    }

    pub async fn get_event_details(
        &self,
        collection_id: &str,
        uid: &str,
        recurrence_id: &str,
    ) -> Option<CalendarEventDetail> {
        self.proxy()
            .await
            .ok()?
            .get_event_details(collection_id, uid, recurrence_id)
            .await
            .ok()
    }

    pub async fn set_collection_visibility(&self, id: &str, visible: bool) -> Result<()> {
        self.proxy()
            .await?
            .set_collection_visibility(id, visible)
            .await
    }

    pub async fn sync(&self) -> Result<()> {
        self.proxy().await?.sync().await
    }
}
