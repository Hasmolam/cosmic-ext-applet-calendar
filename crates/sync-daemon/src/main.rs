use calendar_core::db;
use std::sync::Arc;
use tokio::sync::Mutex;
use zbus::{Connection, Result, SignalContext, fdo, interface};

mod accounts;
mod auth;
mod discovery;
mod secrets;
mod sync;
mod system;

use sync::SyncEngine;
use system::NetworkState;

struct CalendarService {
    database: Arc<Mutex<rusqlite::Connection>>,
    sync_engine: Arc<SyncEngine>,
}

#[interface(name = "dev.cosmic.Calendar")]
impl CalendarService {
    async fn get_collections(&self) -> Vec<calendar_core::CalendarCollection> {
        let db = self.database.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            db::get_collections(&conn).unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    async fn set_collection_visibility(
        &self,
        id: &str,
        visible: bool,
        #[zbus(signal_context)] signal_context: SignalContext<'_>,
    ) {
        let db = self.database.clone();
        let col_id = id.to_string();
        let changed = tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            db::set_collection_visibility(&conn, &col_id, visible).unwrap_or(false)
        })
        .await
        .unwrap_or(false);

        if changed {
            let _ = Self::collections_changed(&signal_context).await;
            let _ = Self::events_changed(&signal_context).await;
        }
    }

    async fn get_events(
        &self,
        start_time: i64,
        end_time: i64,
    ) -> Vec<calendar_core::CalendarEventSummary> {
        let db = self.database.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            db::query_events_for_range(&conn, start_time, end_time).unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    async fn get_event_details(
        &self,
        collection_id: &str,
        uid: &str,
        recurrence_id: &str,
    ) -> calendar_core::CalendarEventDetail {
        let db = self.database.clone();
        let col_id = collection_id.to_string();
        let event_uid = uid.to_string();
        let rec_id = recurrence_id.to_string();

        let detail = tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            db::get_event_detail(&conn, &col_id, &event_uid, &rec_id).unwrap_or(None)
        })
        .await
        .unwrap_or(None);

        detail.unwrap_or(calendar_core::CalendarEventDetail {
            uid: uid.to_string(),
            recurrence_id: recurrence_id.to_string(),
            collection_id: collection_id.to_string(),
            summary: String::new(),
            description: String::new(),
            location: String::new(),
            url: String::new(),
            start_time: 0,
            end_time: 0,
            start_date: String::new(),
            end_date: String::new(),
            is_all_day: false,
        })
    }

    async fn sync(
        &self,
        #[zbus(signal_context)] signal_context: SignalContext<'_>,
    ) -> fdo::Result<()> {
        let _ = Self::sync_status_changed(&signal_context, true).await;
        let sync_engine = self.sync_engine.clone();

        let res = sync_engine.sync_all().await;
        if let Err(e) = res {
            tracing::warn!(?e, "Sync failed");
        }

        let _ = Self::events_changed(&signal_context).await;
        let _ = Self::collections_changed(&signal_context).await;
        let _ = Self::sync_status_changed(&signal_context, false).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn sync_status_changed(
        signal_context: &SignalContext<'_>,
        is_syncing: bool,
    ) -> Result<()>;

    #[zbus(signal)]
    async fn events_changed(signal_context: &SignalContext<'_>) -> Result<()>;

    #[zbus(signal)]
    async fn collections_changed(signal_context: &SignalContext<'_>) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Periodic sync with WAL checkpoint
// ---------------------------------------------------------------------------

async fn run_periodic_sync(
    sync_engine: Arc<SyncEngine>,
    database: Arc<Mutex<rusqlite::Connection>>,
    network: NetworkState,
    signal_ctx: zbus::SignalContext<'static>,
) {
    loop {
        let interval = network.sync_interval_secs();
        tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;

        if !network.sync_allowed() {
            tracing::debug!("Periodic sync skipped — network unavailable");
            continue;
        }

        tracing::debug!("Periodic sync starting");
        let _ = CalendarService::sync_status_changed(&signal_ctx, true).await;

        let res = sync_engine.sync_all().await;
        if let Err(e) = res {
            tracing::warn!(?e, "Periodic sync failed");
        }

        // WAL passive checkpoint: flushes WAL frames without blocking readers.
        let db = database.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db.blocking_lock();
            if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);") {
                tracing::warn!(?e, "WAL checkpoint failed");
            }
        })
        .await
        .ok();

        let _ = CalendarService::events_changed(&signal_ctx).await;
        let _ = CalendarService::collections_changed(&signal_ctx).await;
        let _ = CalendarService::sync_status_changed(&signal_ctx, false).await;
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let database_conn = tokio::task::spawn_blocking(db::open_database)
        .await
        .map_err(|e| zbus::Error::Failure(e.to_string()))?
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    let database = Arc::new(Mutex::new(database_conn));
    let sync_engine = Arc::new(SyncEngine::new(database.clone()));

    let service = CalendarService {
        database: database.clone(),
        sync_engine: sync_engine.clone(),
    };

    let connection = Connection::session().await?;
    connection.request_name("dev.cosmic.Calendar").await?;
    connection
        .object_server()
        .at("/dev/cosmic/Calendar", service)
        .await?;
    tracing::info!("COSMIC Calendar D-Bus service is ready");

    // Obtain a static signal context for use in background tasks.
    let iface_ref = connection
        .object_server()
        .interface::<_, CalendarService>("/dev/cosmic/Calendar")
        .await?;
    let signal_ctx = iface_ref.signal_context().clone();
    // SAFETY: the connection stays alive for the process lifetime.
    let static_ctx: zbus::SignalContext<'static> =
        unsafe { std::mem::transmute(signal_ctx.clone()) };

    // Network state shared between the watcher and the periodic sync loop.
    let network = NetworkState::default();

    // Account event watcher (Probe-Then-Listen model).
    let account_database = database.clone();
    let account_connection = connection.clone();
    let on_appeared_db = database.clone();
    let on_appeared_sync = sync_engine.clone();
    tokio::spawn(async move {
        let on_appeared = move || {
            let db = on_appeared_db.clone();
            let sync = on_appeared_sync.clone();
            async move {
                tracing::info!("Accounts service appeared — running discovery and sync");
                if let Err(error) = accounts::discover_accounts_once(db).await {
                    tracing::debug!(
                        ?error,
                        "Account discovery unavailable after service appeared"
                    );
                } else {
                    let _ = sync.sync_all().await;
                }
            }
        };
        if let Err(error) =
            accounts::watch_account_events(account_connection, account_database, on_appeared).await
        {
            tracing::debug!(?error, "Accounts service watcher stopped");
        }
    });

    // Initial account discovery + first sync.
    let discovery_database = database.clone();
    let discovery_sync = sync_engine.clone();
    tokio::spawn(async move {
        if let Err(error) = accounts::discover_accounts_once(discovery_database).await {
            tracing::debug!(?error, "Initial account discovery unavailable");
        } else {
            let _ = discovery_sync.sync_all().await;
        }
    });

    // Periodic sync loop.
    let periodic_db = database.clone();
    let periodic_engine = sync_engine.clone();
    let periodic_network = network.clone();
    let periodic_ctx = static_ctx.clone();
    tokio::spawn(async move {
        run_periodic_sync(periodic_engine, periodic_db, periodic_network, periodic_ctx).await;
    });

    // Network monitor: pause sync on no connectivity / metered.
    let net_state_for_watcher = network.clone();
    let net_engine_for_reconnect = sync_engine.clone();
    let net_ctx_for_reconnect = static_ctx.clone();
    tokio::spawn(async move {
        let on_reconnect = move || {
            let engine = net_engine_for_reconnect.clone();
            let ctx = net_ctx_for_reconnect.clone();
            async move {
                tracing::info!("Network reconnected — triggering immediate sync");
                let _ = CalendarService::sync_status_changed(&ctx, true).await;
                if let Err(e) = engine.sync_all().await {
                    tracing::warn!(?e, "Post-reconnect sync failed");
                }
                let _ = CalendarService::events_changed(&ctx).await;
                let _ = CalendarService::collections_changed(&ctx).await;
                let _ = CalendarService::sync_status_changed(&ctx, false).await;
            }
        };
        if let Err(e) = system::watch_network(net_state_for_watcher, on_reconnect).await {
            tracing::debug!(?e, "Network monitor unavailable — running without it");
        }
    });

    // Timezone watcher: emit events_changed when the clock zone changes.
    let tz_ctx = static_ctx.clone();
    tokio::spawn(async move {
        let on_tz_change = move |tz: String| {
            let ctx = tz_ctx.clone();
            async move {
                tracing::info!(timezone = %tz, "Timezone changed — emitting events_changed");
                let _ = CalendarService::events_changed(&ctx).await;
            }
        };
        if let Err(e) = system::watch_timezone(on_tz_change).await {
            tracing::debug!(?e, "Timezone monitor unavailable — running without it");
        }
    });

    // Sleep/wake watcher: sync immediately on resume.
    let wake_engine = sync_engine.clone();
    let wake_ctx = static_ctx.clone();
    tokio::spawn(async move {
        let on_wake = move || {
            let engine = wake_engine.clone();
            let ctx = wake_ctx.clone();
            async move {
                tracing::info!("System resumed — triggering background sync");
                let _ = CalendarService::sync_status_changed(&ctx, true).await;
                if let Err(e) = engine.sync_all().await {
                    tracing::warn!(?e, "Post-wake sync failed");
                }
                let _ = CalendarService::events_changed(&ctx).await;
                let _ = CalendarService::collections_changed(&ctx).await;
                let _ = CalendarService::sync_status_changed(&ctx, false).await;
            }
        };
        if let Err(e) = system::watch_sleep(on_wake).await {
            tracing::debug!(?e, "Sleep monitor unavailable — running without it");
        }
    });

    tokio::signal::ctrl_c()
        .await
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(())
}
