#![allow(dead_code)]

use crate::discovery::{CalendarProvider, DiscoveryClient, calendar_root_url};
use crate::secrets::SecretStore;
use calendar_core::db::{self, DatabaseError};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use zbus::{Connection, Result, proxy};

#[allow(dead_code)]
pub const ACCOUNTS_SERVICE: &str = "dev.edfloreshz.Accounts";

#[derive(Clone, Debug, Deserialize, Serialize, zbus::zvariant::Type)]
pub struct AccountRecord {
    pub id: String,
    pub provider: String,
    pub username: String,
    pub server_url: String,
    pub access_token: String,
}

pub const GOOGLE_CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    PermissionDenied,
    NeedsReauth,
}

#[derive(Clone, Debug, Deserialize, Serialize, zbus::zvariant::Type)]
pub struct TokenInfo {
    pub access_token: String,
    pub expires_in: i64,
}

#[derive(Default)]
pub struct AccountStatusTracker {
    statuses: std::sync::Mutex<std::collections::HashMap<String, AccountStatus>>,
    consecutive_auth_failures: std::sync::Mutex<std::collections::HashMap<String, u32>>,
}

impl AccountStatusTracker {
    pub fn get_status(&self, account_id: &str) -> AccountStatus {
        self.statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(account_id)
            .copied()
            .unwrap_or(AccountStatus::Active)
    }

    pub fn set_status(&self, account_id: &str, status: AccountStatus) {
        self.statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(account_id.to_string(), status);
    }

    pub fn record_auth_success(&self, account_id: &str) {
        self.consecutive_auth_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(account_id.to_string(), 0);
        self.set_status(account_id, AccountStatus::Active);
    }

    pub fn record_auth_failure(&self, account_id: &str, is_permanent: bool) -> AccountStatus {
        if !is_permanent {
            return self.get_status(account_id);
        }

        let mut failures = self
            .consecutive_auth_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let count = failures.entry(account_id.to_string()).or_insert(0);
        *count += 1;
        if *count >= 2 {
            self.set_status(account_id, AccountStatus::NeedsReauth);
            AccountStatus::NeedsReauth
        } else {
            AccountStatus::Active
        }
    }
}

#[proxy(
    interface = "dev.edfloreshz.Accounts",
    default_service = "dev.edfloreshz.Accounts",
    default_path = "/dev/edfloreshz/Accounts"
)]
trait AccountsDbus {
    async fn register_handler(&self, service: &str, bus_name: &str) -> Result<()>;
    async fn list_accounts(&self) -> Result<Vec<AccountRecord>>;
    async fn get_access_token(&self, account_id: &str, scope: &str) -> Result<TokenInfo>;
    async fn invalidate_token(&self, account_id: &str, scope: &str) -> Result<()>;

    #[zbus(signal)]
    fn service_enabled(&self, account_id: &str, service: &str) -> Result<()>;

    #[zbus(signal)]
    fn service_disabled(&self, account_id: &str, service: &str) -> Result<()>;

    #[zbus(signal)]
    fn account_removed(&self, account_id: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct AccountsClient {
    connection: Connection,
}

impl AccountsClient {
    pub async fn session() -> Result<Self> {
        Ok(Self {
            connection: Connection::session().await?,
        })
    }

    pub async fn register_handler(&self, service: &str, bus_name: &str) -> Result<()> {
        AccountsDbusProxy::new(&self.connection)
            .await?
            .register_handler(service, bus_name)
            .await
    }

    pub async fn list_accounts(&self) -> Result<Vec<AccountRecord>> {
        AccountsDbusProxy::new(&self.connection)
            .await?
            .list_accounts()
            .await
    }

    pub async fn get_access_token(&self, account_id: &str, scope: &str) -> Result<TokenInfo> {
        AccountsDbusProxy::new(&self.connection)
            .await?
            .get_access_token(account_id, scope)
            .await
    }

    pub async fn invalidate_token(&self, account_id: &str, scope: &str) -> Result<()> {
        AccountsDbusProxy::new(&self.connection)
            .await?
            .invalidate_token(account_id, scope)
            .await
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountServiceEvent {
    ServiceEnabled { account_id: String, service: String },
    ServiceDisabled { account_id: String, service: String },
    AccountRemoved { account_id: String },
}

pub fn apply_account_event(
    connection: &rusqlite::Connection,
    event: AccountServiceEvent,
) -> std::result::Result<usize, DatabaseError> {
    match event {
        AccountServiceEvent::ServiceDisabled {
            account_id,
            service,
        } if service.eq_ignore_ascii_case("calendar") => {
            db::delete_collections_for_account(connection, &account_id)
        }
        AccountServiceEvent::AccountRemoved { account_id } => {
            db::delete_collections_for_account(connection, &account_id)
        }
        _ => Ok(0),
    }
}

pub async fn watch_account_events<F, Fut>(
    connection: Connection,
    database: std::sync::Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    on_service_appeared: F,
) -> Result<()>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let proxy = AccountsDbusProxy::new(&connection).await?;

    // Probe: register calendar handler (best effort, triggers activation if supported)
    let _ = proxy
        .register_handler("calendar", "dev.cosmic.Calendar")
        .await;

    // Probe-Then-Listen: Monitor NameOwnerChanged for dev.edfloreshz.Accounts
    let conn_clone = connection.clone();
    tokio::spawn(async move {
        if let Ok(dbus_proxy) = zbus::fdo::DBusProxy::new(&conn_clone).await
            && let Ok(mut stream) = dbus_proxy.receive_name_owner_changed().await
        {
            while let Some(signal) = stream.next().await {
                if let Ok(args) = signal.args()
                    && args.name() == ACCOUNTS_SERVICE
                    && args.new_owner().is_some()
                {
                    tracing::info!(
                        "dev.edfloreshz.Accounts appeared on D-Bus — running account discovery"
                    );
                    on_service_appeared().await;
                }
            }
        }
    });

    watch_account_events_with_proxy(proxy, database).await
}

pub async fn watch_account_events_with_proxy<'a>(
    proxy: AccountsDbusProxy<'a>,
    database: std::sync::Arc<tokio::sync::Mutex<rusqlite::Connection>>,
) -> Result<()> {
    let mut service_enabled = proxy.receive_service_enabled().await?;
    let mut service_disabled = proxy.receive_service_disabled().await?;
    let mut account_removed = proxy.receive_account_removed().await?;

    loop {
        tokio::select! {
            Some(signal) = service_enabled.next() => {
                let args = signal.args()?;
                if args.service().eq_ignore_ascii_case("calendar") {
                    let account_id = args.account_id().to_string();
                    tracing::info!(account_id = %account_id, "Calendar service enabled; initiating account discovery");
                    let db = database.clone();
                    tokio::spawn(async move {
                        if let Err(error) = discover_single_account(db, &account_id).await {
                            tracing::warn!(account_id = %account_id, ?error, "Account discovery failed after service enabled");
                        }
                    });
                }
            }
            Some(signal) = service_disabled.next() => {
                let args = signal.args()?;
                let event = AccountServiceEvent::ServiceDisabled {
                    account_id: args.account_id().to_string(),
                    service: args.service().to_string(),
                };
                let db = database.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = db.blocking_lock();
                    apply_account_event(&conn, event)
                })
                .await
                .map_err(|error| zbus::Error::Failure(error.to_string()))?
                .map_err(|error| zbus::Error::Failure(error.to_string()))?;
            }
            Some(signal) = account_removed.next() => {
                let args = signal.args()?;
                let event = AccountServiceEvent::AccountRemoved {
                    account_id: args.account_id().to_string(),
                };
                let db = database.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = db.blocking_lock();
                    apply_account_event(&conn, event)
                })
                .await
                .map_err(|error| zbus::Error::Failure(error.to_string()))?
                .map_err(|error| zbus::Error::Failure(error.to_string()))?;
            }
            else => break,
        }
    }
    Ok(())
}

pub async fn discover_single_account(
    database: std::sync::Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    target_account_id: &str,
) -> std::result::Result<(), String> {
    let accounts = AccountsClient::session()
        .await
        .map_err(|error| error.to_string())?;
    let records = accounts
        .list_accounts()
        .await
        .map_err(|error| error.to_string())?;
    let account = records
        .into_iter()
        .find(|a| a.id == target_account_id)
        .ok_or_else(|| format!("Account {target_account_id} not found in Accounts service"))?;

    discover_and_persist_account(database, account).await
}

pub async fn discover_accounts_once(
    database: std::sync::Arc<tokio::sync::Mutex<rusqlite::Connection>>,
) -> std::result::Result<(), String> {
    let accounts = AccountsClient::session()
        .await
        .map_err(|error| error.to_string())?;
    let records = accounts
        .list_accounts()
        .await
        .map_err(|error| error.to_string())?;

    for account in records {
        if let Err(error) = discover_and_persist_account(database.clone(), account).await {
            tracing::warn!(?error, "Failed discovering account during full sync");
        }
    }
    Ok(())
}

async fn discover_and_persist_account(
    database: std::sync::Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    account: AccountRecord,
) -> std::result::Result<(), String> {
    let provider = if account.provider.eq_ignore_ascii_case("google") {
        CalendarProvider::Google
    } else {
        CalendarProvider::CalDav
    };
    let root = match calendar_root_url(
        provider.clone(),
        (!account.server_url.is_empty()).then_some(account.server_url.as_str()),
    ) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(account_id = %account.id, ?error, "Skipping account with invalid calendar URL");
            return Ok(());
        }
    };

    let http = reqwest::Client::new();
    let auth_coordinator = std::sync::Arc::new(crate::auth::SingleFlightAuth::default());
    let discovery = DiscoveryClient::with_auth_coordinator(http, auth_coordinator);

    let discovery_result = if !account.access_token.is_empty() {
        let account_id = account.id.clone();
        let token = account.access_token.clone();
        discovery
            .discover_with_bearer_retry(root, &account.id, &account.access_token, move || {
                let account_id = account_id.clone();
                let initial_token = token.clone();
                async move {
                    if let Ok(client) = AccountsClient::session().await
                        && let Ok(accounts) = client.list_accounts().await
                        && let Some(token) = accounts
                            .into_iter()
                            .find(|a| a.id == account_id)
                            .map(|a| a.access_token)
                            .filter(|t| !t.is_empty() && *t != initial_token)
                    {
                        return Ok(token);
                    }
                    Err(crate::auth::AuthError(
                        "Unable to refresh token from accounts service".to_string(),
                    ))
                }
            })
            .await
    } else if provider == CalendarProvider::CalDav {
        let store = match SecretStore::connect().await {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(account_id = %account.id, ?error, "Secret Service is unavailable");
                return Ok(());
            }
        };
        let password = match store.load_password(&account.id).await {
            Ok(Some(password)) => password,
            Ok(None) => {
                tracing::warn!(account_id = %account.id, "No CalDAV password found in Secret Service");
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(account_id = %account.id, ?error, "Unable to load CalDAV password");
                return Ok(());
            }
        };
        discovery
            .discover_with_basic(root, &account.username, &password)
            .await
    } else {
        discovery.discover_with_bearer(root, None).await
    };

    let result = match discovery_result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(account_id = %account.id, ?error, "Calendar discovery failed");
            return Ok(());
        }
    };

    let account_id = account.id;
    let records: Vec<calendar_core::DiscoveredCollectionRecord> = result
        .collections
        .into_iter()
        .map(|col| calendar_core::DiscoveredCollectionRecord {
            href: col.href.to_string(),
            display_name: col.display_name,
            color: col.color,
            ctag: col.ctag,
        })
        .collect();

    let discovered_hrefs: Vec<String> = records.iter().map(|r| r.href.clone()).collect();
    let now = unix_timestamp();

    tokio::task::spawn_blocking(move || {
        let connection = database.blocking_lock();
        db::upsert_discovered_collections(&connection, &account_id, &records, now)
            .map_err(|error| error.to_string())?;
        db::delete_missing_collections(&connection, &account_id, &discovered_hrefs)
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(())
    })
    .await
    .map_err(|error| error.to_string())??;

    Ok(())
}

fn unix_timestamp() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection as SqliteConnection;
    use rusqlite_migration::{M, Migrations};
    use zbus::{Connection as ZbusConnection, SignalContext, interface};

    fn test_db_connection() -> SqliteConnection {
        let mut connection = SqliteConnection::open_in_memory().unwrap();
        Migrations::new(vec![M::up(
            "CREATE TABLE collections (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, href TEXT NOT NULL, display_name TEXT, color TEXT, ctag TEXT, sync_token TEXT, is_visible INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL);\
             CREATE TABLE events (collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE, uid TEXT NOT NULL, href TEXT NOT NULL, etag TEXT NOT NULL, start_time INTEGER NOT NULL, end_time INTEGER NOT NULL, raw_ics TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (collection_id, uid));\
             PRAGMA foreign_keys = ON;",
        )])
        .to_latest(&mut connection)
        .unwrap();
        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, updated_at) VALUES ('calendar-a', 'account-a', 'https://calendar.test/a', 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events (collection_id, uid, href, etag, start_time, end_time, raw_ics, updated_at) VALUES ('calendar-a', 'event-a', '/event.ics', 'etag', 1, 2, 'BEGIN:VCALENDAR', 1)",
                [],
            )
            .unwrap();
        connection
    }

    #[test]
    fn calendar_service_disable_removes_account_data() {
        let connection = test_db_connection();
        let deleted = apply_account_event(
            &connection,
            AccountServiceEvent::ServiceDisabled {
                account_id: "account-a".to_string(),
                service: "calendar".to_string(),
            },
        )
        .unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn non_calendar_service_disable_is_ignored() {
        let connection = test_db_connection();
        assert_eq!(
            apply_account_event(
                &connection,
                AccountServiceEvent::ServiceDisabled {
                    account_id: "account-a".to_string(),
                    service: "mail".to_string(),
                },
            )
            .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM collections", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn account_removed_deletes_collections_and_events() {
        let connection = test_db_connection();
        let deleted = apply_account_event(
            &connection,
            AccountServiceEvent::AccountRemoved {
                account_id: "account-a".to_string(),
            },
        )
        .unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM collections", [], |row| row
                    .get::<_, i64>(0))
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
    }

    struct MockAccountsService {
        accounts: Vec<AccountRecord>,
    }

    #[interface(name = "dev.edfloreshz.Accounts")]
    impl MockAccountsService {
        async fn register_handler(&self, _service: &str, _bus_name: &str) {}

        async fn list_accounts(&self) -> Vec<AccountRecord> {
            self.accounts.clone()
        }

        async fn get_access_token(&self, account_id: &str, _scope: &str) -> TokenInfo {
            let token = self
                .accounts
                .iter()
                .find(|a| a.id == account_id)
                .map(|a| a.access_token.clone())
                .unwrap_or_default();
            TokenInfo {
                access_token: token,
                expires_in: 3600,
            }
        }

        async fn invalidate_token(&self, _account_id: &str, _scope: &str) {}

        #[zbus(signal)]
        async fn service_enabled(
            signal_context: &SignalContext<'_>,
            account_id: &str,
            service: &str,
        ) -> Result<()>;

        #[zbus(signal)]
        async fn service_disabled(
            signal_context: &SignalContext<'_>,
            account_id: &str,
            service: &str,
        ) -> Result<()>;

        #[zbus(signal)]
        async fn account_removed(
            signal_context: &SignalContext<'_>,
            account_id: &str,
        ) -> Result<()>;
    }

    #[tokio::test]
    async fn live_dbus_accounts_contract_and_signals() {
        let server = MockAccountsService {
            accounts: vec![AccountRecord {
                id: "test-acc-1".to_string(),
                provider: "google".to_string(),
                username: "test@gmail.com".to_string(),
                server_url: String::new(),
                access_token: "token-123".to_string(),
            }],
        };

        let server_conn = ZbusConnection::session().await.unwrap();
        let server_path = "/dev/edfloreshz/Accounts/Test";
        server_conn
            .object_server()
            .at(server_path, server)
            .await
            .unwrap();

        let unique_name = server_conn.unique_name().unwrap();

        let client_conn = ZbusConnection::session().await.unwrap();
        let proxy = AccountsDbusProxy::builder(&client_conn)
            .destination(unique_name.as_str())
            .unwrap()
            .path(server_path)
            .unwrap()
            .build()
            .await
            .unwrap();

        let accounts = proxy.list_accounts().await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "test-acc-1");
        assert_eq!(accounts[0].provider, "google");
        assert_eq!(accounts[0].username, "test@gmail.com");
        assert_eq!(accounts[0].access_token, "token-123");

        let mut enabled_stream = proxy.receive_service_enabled().await.unwrap();
        let mut disabled_stream = proxy.receive_service_disabled().await.unwrap();
        let mut removed_stream = proxy.receive_account_removed().await.unwrap();

        let iface_ref = server_conn
            .object_server()
            .interface::<_, MockAccountsService>(server_path)
            .await
            .unwrap();
        let signal_ctxt = iface_ref.signal_context();

        MockAccountsService::service_enabled(signal_ctxt, "test-acc-1", "calendar")
            .await
            .unwrap();
        let sig = enabled_stream.next().await.unwrap();
        let args = sig.args().unwrap();
        assert_eq!(*args.account_id(), "test-acc-1");
        assert_eq!(*args.service(), "calendar");

        MockAccountsService::service_disabled(signal_ctxt, "test-acc-1", "calendar")
            .await
            .unwrap();
        let sig = disabled_stream.next().await.unwrap();
        let args = sig.args().unwrap();
        assert_eq!(*args.account_id(), "test-acc-1");
        assert_eq!(*args.service(), "calendar");

        MockAccountsService::account_removed(signal_ctxt, "test-acc-1")
            .await
            .unwrap();
        let sig = removed_stream.next().await.unwrap();
        let args = sig.args().unwrap();
        assert_eq!(*args.account_id(), "test-acc-1");
    }

    #[tokio::test]
    async fn watcher_reacts_to_dbus_signals_via_spawn_blocking() {
        let server = MockAccountsService {
            accounts: vec![AccountRecord {
                id: "test-acc-watcher".to_string(),
                provider: "google".to_string(),
                username: "watcher@test.com".to_string(),
                server_url: String::new(),
                access_token: "token-watcher".to_string(),
            }],
        };

        let server_conn = ZbusConnection::session().await.unwrap();
        let server_path = "/dev/edfloreshz/Accounts/TestWatcher";
        server_conn
            .object_server()
            .at(server_path, server)
            .await
            .unwrap();

        let unique_name = server_conn.unique_name().unwrap().to_owned();

        let client_conn = ZbusConnection::session().await.unwrap();
        let proxy = AccountsDbusProxy::builder(&client_conn)
            .destination(unique_name)
            .unwrap()
            .path(server_path)
            .unwrap()
            .build()
            .await
            .unwrap();

        let db = test_db_connection();
        db.execute(
            "INSERT INTO collections (id, account_id, href, updated_at) VALUES ('c1', 'test-acc-watcher', 'https://test/c1', 1)",
            [],
        )
        .unwrap();

        let db_arc = std::sync::Arc::new(tokio::sync::Mutex::new(db));

        let watcher_db = db_arc.clone();
        let watcher_handle = tokio::spawn(async move {
            let _ = watch_account_events_with_proxy(proxy, watcher_db).await;
        });

        let iface_ref = server_conn
            .object_server()
            .interface::<_, MockAccountsService>(server_path)
            .await
            .unwrap();
        let signal_ctxt = iface_ref.signal_context();

        // Allow watcher AddMatch D-Bus registration to establish
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        MockAccountsService::service_disabled(signal_ctxt, "test-acc-watcher", "calendar")
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        {
            let conn = db_arc.lock().await;
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM collections WHERE account_id = 'test-acc-watcher'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "Collections should be deleted on ServiceDisabled");
        }

        watcher_handle.abort();
    }

    #[test]
    fn google_calendar_scope_is_strictly_readonly() {
        assert_eq!(
            GOOGLE_CALENDAR_SCOPE,
            "https://www.googleapis.com/auth/calendar.readonly"
        );
    }

    #[test]
    fn account_status_tracker_tracks_permanent_failures_and_needs_reauth() {
        let tracker = AccountStatusTracker::default();
        let acc = "test-acc-reauth";

        // Initial status is Active
        assert_eq!(tracker.get_status(acc), AccountStatus::Active);

        // Network error (not permanent): counter does not increase
        let status = tracker.record_auth_failure(acc, false);
        assert_eq!(status, AccountStatus::Active);
        assert_eq!(tracker.get_status(acc), AccountStatus::Active);

        // 1st permanent auth error
        let status = tracker.record_auth_failure(acc, true);
        assert_eq!(status, AccountStatus::Active);

        // 2nd permanent auth error -> NeedsReauth
        let status = tracker.record_auth_failure(acc, true);
        assert_eq!(status, AccountStatus::NeedsReauth);
        assert_eq!(tracker.get_status(acc), AccountStatus::NeedsReauth);

        // Success resets status to Active
        tracker.record_auth_success(acc);
        assert_eq!(tracker.get_status(acc), AccountStatus::Active);
    }
}
