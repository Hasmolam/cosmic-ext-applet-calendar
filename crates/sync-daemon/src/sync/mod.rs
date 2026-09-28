pub mod caldav;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use calendar_core::db;
use calendar_core::ical::parse_ics_events;
use tokio::sync::Mutex;
use url::Url;

use self::caldav::{CalDavClient, CalDavSyncError, SyncAuth, SyncItemChange};
use crate::accounts::{AccountRecord, AccountsClient};
use crate::auth::SingleFlightAuth;
use crate::discovery::CalendarProvider;
use crate::secrets::SecretStore;

pub fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub struct SyncCollectionContext<'a> {
    pub account: &'a AccountRecord,
    pub auth: &'a SyncAuth,
    pub collection_id: &'a str,
    pub col_url: &'a Url,
}

pub struct SyncEngine {
    db: Arc<Mutex<rusqlite::Connection>>,
    #[allow(dead_code)]
    auth_coordinator: Arc<SingleFlightAuth>,
    client: CalDavClient,
}

impl SyncEngine {
    pub fn new(db: Arc<Mutex<rusqlite::Connection>>) -> Self {
        let auth_coordinator = Arc::new(SingleFlightAuth::default());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let client = CalDavClient::with_auth_coordinator(http, auth_coordinator.clone());

        Self {
            db,
            auth_coordinator,
            client,
        }
    }

    pub async fn sync_all(&self) -> Result<(), String> {
        let accounts = AccountsClient::session()
            .await
            .map_err(|e| e.to_string())?
            .list_accounts()
            .await
            .map_err(|e| e.to_string())?;

        for account in accounts {
            if let Err(e) = self.sync_account(&account).await {
                tracing::warn!(account_id = %account.id, ?e, "Failed syncing account");
            }
        }

        Ok(())
    }

    pub async fn sync_account(&self, account: &AccountRecord) -> Result<(), String> {
        let provider = if account.provider.eq_ignore_ascii_case("google") {
            CalendarProvider::Google
        } else {
            CalendarProvider::CalDav
        };

        let auth = if !account.access_token.is_empty() {
            SyncAuth::Bearer(account.access_token.clone())
        } else if provider == CalendarProvider::CalDav {
            let store = SecretStore::connect().await.map_err(|e| e.to_string())?;
            let password = store
                .load_password(&account.id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("No CalDAV password found for {}", account.id))?;
            SyncAuth::Basic(account.username.clone(), password)
        } else {
            return Err(format!("No auth available for {}", account.id));
        };

        // Query collections for account from DB
        let account_id = account.id.clone();
        let db_for_query = self.db.clone();
        let collections: Vec<(String, String, Option<String>)> =
            tokio::task::spawn_blocking(move || {
                let conn = db_for_query.blocking_lock();
                let mut stmt = conn
                    .prepare("SELECT id, href, sync_token FROM collections WHERE account_id = ?1")
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([&account_id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .map_err(|e| e.to_string())?;
                let mut list = Vec::new();
                for r in rows {
                    list.push(r.map_err(|e| e.to_string())?);
                }
                Ok::<_, String>(list)
            })
            .await
            .map_err(|e| e.to_string())??;

        for (col_id, col_href, sync_token) in collections {
            if let Err(err) = self
                .sync_single_collection(account, &auth, &col_id, &col_href, sync_token.as_deref())
                .await
            {
                tracing::warn!(col_id = %col_id, ?err, "Failed syncing collection");
            }
        }

        Ok(())
    }

    pub async fn sync_single_collection(
        &self,
        account: &AccountRecord,
        auth: &SyncAuth,
        collection_id: &str,
        collection_href: &str,
        sync_token: Option<&str>,
    ) -> Result<(), String> {
        let col_url = Url::parse(collection_href).map_err(|e| e.to_string())?;
        let account_id = account.id.clone();
        let initial_token = account.access_token.clone();

        let refresh = move || {
            let account_id = account_id.clone();
            let initial_token = initial_token.clone();
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
        };

        // Sync Horizon: today - 365 days (for initial sync when sync_token is None)
        let now = unix_timestamp();
        let sync_horizon_epoch = if sync_token.is_none() {
            Some(now - (365 * 86400))
        } else {
            None
        };

        // Call sync_collection
        let sync_result = self
            .client
            .sync_collection(
                &col_url,
                &account.id,
                auth,
                sync_token,
                sync_horizon_epoch,
                refresh.clone(),
            )
            .await;

        match sync_result {
            Ok(result) => {
                let ctx = SyncCollectionContext {
                    account,
                    auth,
                    collection_id,
                    col_url: &col_url,
                };
                self.process_sync_changes(&ctx, result.new_sync_token, result.changes, refresh)
                    .await?;
            }
            Err(CalDavSyncError::SyncTokenExpired) => {
                tracing::warn!(
                    collection_id = %collection_id,
                    "Sync token expired (HTTP 410 Gone); resetting collection and triggering full sync"
                );
                let db_clone = self.db.clone();
                let col_id_clone = collection_id.to_string();
                let updated_now = unix_timestamp();

                tokio::task::spawn_blocking(move || {
                    let conn = db_clone.blocking_lock();
                    db::reset_collection_for_full_sync(&conn, &col_id_clone, updated_now)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;

                // Re-run initial sync without sync_token
                let initial_horizon = Some(now - (365 * 86400));
                let full_sync_res = self
                    .client
                    .sync_collection(
                        &col_url,
                        &account.id,
                        auth,
                        None,
                        initial_horizon,
                        refresh.clone(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                let ctx = SyncCollectionContext {
                    account,
                    auth,
                    collection_id,
                    col_url: &col_url,
                };
                self.process_sync_changes(
                    &ctx,
                    full_sync_res.new_sync_token,
                    full_sync_res.changes,
                    refresh,
                )
                .await?;
            }
            Err(e) => return Err(e.to_string()),
        }

        Ok(())
    }

    async fn process_sync_changes<F, Fut>(
        &self,
        ctx: &SyncCollectionContext<'_>,
        new_sync_token: String,
        changes: Vec<SyncItemChange>,
        refresh: F,
    ) -> Result<(), String>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, crate::auth::AuthError>> + Send + 'static,
    {
        let now = unix_timestamp();

        // Zero-changes rule (PLAN.md 5.6): if changes are empty, update sync_token immediately and return
        if changes.is_empty() {
            if !new_sync_token.is_empty() {
                let db_clone = self.db.clone();
                let col_id = ctx.collection_id.to_string();
                let token = new_sync_token.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = db_clone.blocking_lock();
                    db::update_sync_token(&conn, &col_id, &token, now)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }

        let mut hrefs_to_fetch = Vec::new();
        let mut hrefs_to_delete = Vec::new();

        for change in changes {
            match change {
                SyncItemChange::Updated { href, .. } => hrefs_to_fetch.push(href),
                SyncItemChange::Deleted { href } => hrefs_to_delete.push(href),
            }
        }

        // Apply deletions first
        if !hrefs_to_delete.is_empty() {
            let db_clone = self.db.clone();
            let col_id = ctx.collection_id.to_string();
            let to_delete = hrefs_to_delete.clone();
            tokio::task::spawn_blocking(move || {
                let conn = db_clone.blocking_lock();
                for href in to_delete {
                    let _ = db::delete_event_file(&conn, &col_id, &href);
                }
            })
            .await
            .map_err(|e| e.to_string())?;
        }

        // Fetch changed items via chunked multiget (RAM buffered)
        if !hrefs_to_fetch.is_empty() {
            let multiget_items = self
                .client
                .multiget(
                    ctx.col_url,
                    &ctx.account.id,
                    ctx.auth,
                    &hrefs_to_fetch,
                    Arc::new(refresh),
                )
                .await
                .map_err(|e| e.to_string())?;

            // RAM parse all ICS data before touching database
            let mut parsed_by_href = Vec::new();
            for item in multiget_items {
                let records = parse_ics_events(
                    &item.ics_data,
                    ctx.collection_id,
                    &item.href,
                    &item.etag,
                    now,
                );
                parsed_by_href.push((item.href, records));
            }

            // Write all parsed records atomically per href in SQLite
            let db_clone = self.db.clone();
            let col_id = ctx.collection_id.to_string();
            tokio::task::spawn_blocking(move || {
                let conn = db_clone.blocking_lock();
                for (href, events) in parsed_by_href {
                    db::atomic_replace_events(&conn, &col_id, &href, &events, now)
                        .map_err(|e| e.to_string())?;
                }
                Ok::<_, String>(())
            })
            .await
            .map_err(|e| e.to_string())??;
        }

        // After all chunks applied, update sync_token in SQLite
        if !new_sync_token.is_empty() {
            let db_clone = self.db.clone();
            let col_id = ctx.collection_id.to_string();
            let token = new_sync_token.clone();
            tokio::task::spawn_blocking(move || {
                let conn = db_clone.blocking_lock();
                db::update_sync_token(&conn, &col_id, &token, now)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_process_sync_changes_zero_changes_updates_token() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        rusqlite_migration::Migrations::new(vec![rusqlite_migration::M::up(
            "CREATE TABLE collections (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, href TEXT NOT NULL, display_name TEXT, color TEXT, ctag TEXT, sync_token TEXT, is_visible INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL);\
             CREATE TABLE events (collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE, uid TEXT NOT NULL, recurrence_id TEXT NOT NULL DEFAULT '', recurrence_time INTEGER, href TEXT NOT NULL, etag TEXT NOT NULL, summary TEXT, description TEXT, location TEXT, url TEXT, start_time INTEGER NOT NULL, end_time INTEGER NOT NULL, start_date TEXT, end_date TEXT, duration_days INTEGER NOT NULL DEFAULT 0, is_all_day INTEGER NOT NULL DEFAULT 0, tzid TEXT, rrule TEXT, rrule_until INTEGER, exdate TEXT, raw_ics TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'CONFIRMED', updated_at INTEGER NOT NULL, PRIMARY KEY (collection_id, uid, recurrence_id));"
        )]).to_latest(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, sync_token, updated_at) VALUES ('col1', 'acc1', 'https://cal.test/col1', 'old-token', 1)",
                [],
            )
            .unwrap();

        let db = Arc::new(Mutex::new(connection));
        let sync_engine = SyncEngine::new(db.clone());

        let account = AccountRecord {
            id: "acc1".to_string(),
            provider: "google".to_string(),
            username: "user@test.com".to_string(),
            server_url: String::new(),
            access_token: "token".to_string(),
        };

        let col_url = Url::parse("https://cal.test/col1").unwrap();

        let ctx = SyncCollectionContext {
            account: &account,
            auth: &SyncAuth::Bearer("token".to_string()),
            collection_id: "col1",
            col_url: &col_url,
        };

        sync_engine
            .process_sync_changes(&ctx, "new-token-123".to_string(), Vec::new(), || async {
                Ok("token".to_string())
            })
            .await
            .unwrap();

        let token: String = db
            .lock()
            .await
            .query_row(
                "SELECT sync_token FROM collections WHERE id = 'col1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(token, "new-token-123");
    }

    #[tokio::test]
    async fn test_process_sync_changes_deletions() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        rusqlite_migration::Migrations::new(vec![rusqlite_migration::M::up(
            "CREATE TABLE collections (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, href TEXT NOT NULL, display_name TEXT, color TEXT, ctag TEXT, sync_token TEXT, is_visible INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL);\
             CREATE TABLE events (collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE, uid TEXT NOT NULL, recurrence_id TEXT NOT NULL DEFAULT '', recurrence_time INTEGER, href TEXT NOT NULL, etag TEXT NOT NULL, summary TEXT, description TEXT, location TEXT, url TEXT, start_time INTEGER NOT NULL, end_time INTEGER NOT NULL, start_date TEXT, end_date TEXT, duration_days INTEGER NOT NULL DEFAULT 0, is_all_day INTEGER NOT NULL DEFAULT 0, tzid TEXT, rrule TEXT, rrule_until INTEGER, exdate TEXT, raw_ics TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'CONFIRMED', updated_at INTEGER NOT NULL, PRIMARY KEY (collection_id, uid, recurrence_id));"
        )]).to_latest(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO collections (id, account_id, href, sync_token, updated_at) VALUES ('col1', 'acc1', 'https://cal.test/col1', 'old-token', 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events (collection_id, uid, href, etag, start_time, end_time, raw_ics, updated_at) VALUES ('col1', 'ev1', '/col1/ev1.ics', 'etag', 1, 2, 'ICS', 1)",
                [],
            )
            .unwrap();

        let db = Arc::new(Mutex::new(connection));
        let sync_engine = SyncEngine::new(db.clone());

        let account = AccountRecord {
            id: "acc1".to_string(),
            provider: "google".to_string(),
            username: "user@test.com".to_string(),
            server_url: String::new(),
            access_token: "token".to_string(),
        };

        let col_url = Url::parse("https://cal.test/col1").unwrap();

        let ctx = SyncCollectionContext {
            account: &account,
            auth: &SyncAuth::Bearer("token".to_string()),
            collection_id: "col1",
            col_url: &col_url,
        };

        sync_engine
            .process_sync_changes(
                &ctx,
                "token-after-deletion".to_string(),
                vec![SyncItemChange::Deleted {
                    href: "/col1/ev1.ics".to_string(),
                }],
                || async { Ok("token".to_string()) },
            )
            .await
            .unwrap();

        let conn = db.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE collection_id = 'col1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        let token: String = conn
            .query_row(
                "SELECT sync_token FROM collections WHERE id = 'col1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(token, "token-after-deletion");
    }

    #[tokio::test]
    async fn test_wiremock_caldav_full_cycle_propfind_sync_and_multiget() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let server_uri = server.uri();

        // 1. PROPFIND for principal
        Mock::given(method("PROPFIND"))
            .and(path("/remote.php/dav/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
    <d:response>
        <d:href>/remote.php/dav/</d:href>
        <d:propstat>
            <d:prop>
                <d:current-user-principal>
                    <d:href>{server_uri}/remote.php/dav/principals/users/reviewer/</d:href>
                </d:current-user-principal>
            </d:prop>
            <d:status>HTTP/1.1 200 OK</d:status>
        </d:propstat>
    </d:response>
</d:multistatus>"#
            )))
            .mount(&server)
            .await;

        // 2. PROPFIND for calendar-home-set
        Mock::given(method("PROPFIND"))
            .and(path("/remote.php/dav/principals/users/reviewer/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
    <d:response>
        <d:href>/remote.php/dav/principals/users/reviewer/</d:href>
        <d:propstat>
            <d:prop>
                <c:calendar-home-set>
                    <d:href>{server_uri}/remote.php/dav/calendars/reviewer/</d:href>
                </c:calendar-home-set>
            </d:prop>
            <d:status>HTTP/1.1 200 OK</d:status>
        </d:propstat>
    </d:response>
</d:multistatus>"#
            )))
            .mount(&server)
            .await;

        // 3. PROPFIND for collections
        Mock::given(method("PROPFIND"))
            .and(path("/remote.php/dav/calendars/reviewer/"))
            .respond_with(ResponseTemplate::new(207).set_body_string(format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/">
    <d:response>
        <d:href>{server_uri}/remote.php/dav/calendars/reviewer/personal/</d:href>
        <d:propstat>
            <d:prop>
                <d:resourcetype>
                    <d:collection/>
                    <c:calendar/>
                </d:resourcetype>
                <d:displayname>Reviewer Calendar</d:displayname>
                <a:calendar-color>#7aa2f7</a:calendar-color>
                <c:getctag>ctag-v1</c:getctag>
            </d:prop>
            <d:status>HTTP/1.1 200 OK</d:status>
        </d:propstat>
    </d:response>
</d:multistatus>"#
            )))
            .mount(&server)
            .await;

        // 4. REPORT for calendar-query (initial sync)
        Mock::given(method("REPORT"))
            .and(path("/remote.php/dav/calendars/reviewer/personal/"))
            .and(body_string_contains("calendar-query"))
            .respond_with(ResponseTemplate::new(207).set_body_string(format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
    <d:sync-token>sync-token-wiremock-100</d:sync-token>
    <d:response>
        <d:href>{server_uri}/remote.php/dav/calendars/reviewer/personal/event1.ics</d:href>
        <d:propstat>
            <d:status>HTTP/1.1 200 OK</d:status>
        </d:propstat>
    </d:response>
</d:multistatus>"#
            )))
            .mount(&server)
            .await;

        // 4b. REPORT for subsequent sync-collection
        Mock::given(method("REPORT"))
            .and(path("/remote.php/dav/calendars/reviewer/personal/"))
            .and(body_string_contains("sync-collection"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
    <d:sync-token>sync-token-wiremock-100</d:sync-token>
</d:multistatus>"#,
            ))
            .mount(&server)
            .await;

        // 5. REPORT for multiget
        Mock::given(method("REPORT"))
            .and(path("/remote.php/dav/calendars/reviewer/personal/"))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(ResponseTemplate::new(207).set_body_string(format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
    <d:response>
        <d:href>{server_uri}/remote.php/dav/calendars/reviewer/personal/event1.ics</d:href>
        <d:propstat>
            <d:prop>
                <d:getetag>"etag-wiremock-1"</d:getetag>
                <c:calendar-data><![CDATA[BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//COSMIC//Reviewer Test//EN
BEGIN:VEVENT
UID:wiremock-reviewer-vevent-1
DTSTAMP:20261001T090000Z
DTSTART:20261001T100000Z
DTEND:20261001T110000Z
SUMMARY:Reviewer Automated Wiremock Meeting
LOCATION:https://meet.google.com/rvw-test-mtg
DESCRIPTION:Zero-credentials mock test meeting
END:VEVENT
END:VCALENDAR]]></c:calendar-data>
            </d:prop>
            <d:status>HTTP/1.1 200 OK</d:status>
        </d:propstat>
    </d:response>
</d:multistatus>"#
            )))
            .mount(&server)
            .await;

        // Test discovery flow
        let http = reqwest::Client::new();
        let discovery = crate::discovery::DiscoveryClient::new(http);
        let root_url = Url::parse(&format!("{server_uri}/remote.php/dav/")).unwrap();
        let res = discovery
            .discover_with_bearer(root_url, Some("mock-token"))
            .await
            .unwrap();
        assert_eq!(res.collections.len(), 1);
        assert_eq!(
            res.collections[0].display_name.as_deref(),
            Some("Reviewer Calendar")
        );

        // Set up in-memory DB and populate collection
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        rusqlite_migration::Migrations::new(vec![rusqlite_migration::M::up(
            "CREATE TABLE collections (id TEXT PRIMARY KEY, account_id TEXT NOT NULL, href TEXT NOT NULL, display_name TEXT, color TEXT, ctag TEXT, sync_token TEXT, is_visible INTEGER NOT NULL DEFAULT 1, updated_at INTEGER NOT NULL);\
             CREATE TABLE events (collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE, uid TEXT NOT NULL, recurrence_id TEXT NOT NULL DEFAULT '', recurrence_time INTEGER, href TEXT NOT NULL, etag TEXT NOT NULL, summary TEXT, description TEXT, location TEXT, url TEXT, start_time INTEGER NOT NULL, end_time INTEGER NOT NULL, start_date TEXT, end_date TEXT, duration_days INTEGER NOT NULL DEFAULT 0, is_all_day INTEGER NOT NULL DEFAULT 0, tzid TEXT, rrule TEXT, rrule_until INTEGER, exdate TEXT, raw_ics TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'CONFIRMED', updated_at INTEGER NOT NULL, PRIMARY KEY (collection_id, uid, recurrence_id));"
        )]).to_latest(&mut connection).unwrap();

        let col_url_str = res.collections[0].href.to_string();
        connection.execute(
            "INSERT INTO collections (id, account_id, href, display_name, color, ctag, is_visible, updated_at)
             VALUES ('col-mock-1', 'acc-mock', ?1, 'Reviewer Calendar', '#7aa2f7', 'ctag-v1', 1, 1)",
            [&col_url_str],
        ).unwrap();

        let db = Arc::new(Mutex::new(connection));
        let sync_engine = SyncEngine::new(db.clone());

        let account = AccountRecord {
            id: "acc-mock".to_string(),
            provider: "caldav".to_string(),
            username: "reviewer".to_string(),
            server_url: server_uri,
            access_token: "mock-token".to_string(),
        };

        let auth = SyncAuth::Bearer("mock-token".to_string());

        // Run sync on the collection
        sync_engine
            .sync_single_collection(
                &account,
                &auth,
                "col-mock-1",
                &col_url_str,
                None, // initial sync
            )
            .await
            .unwrap();

        // Verify SQLite state
        let conn = db.lock().await;
        let event_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE collection_id = 'col-mock-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            event_count, 1,
            "Expected 1 event inserted via mock CalDAV sync"
        );

        let (summary, url, uid): (String, Option<String>, String) = conn
            .query_row(
                "SELECT summary, url, uid FROM events WHERE collection_id = 'col-mock-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(summary, "Reviewer Automated Wiremock Meeting");
        assert_eq!(url.as_deref(), Some("https://meet.google.com/rvw-test-mtg"));
        assert_eq!(uid, "wiremock-reviewer-vevent-1");

        let token: String = conn
            .query_row(
                "SELECT sync_token FROM collections WHERE id = 'col-mock-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(token, "sync-token-wiremock-100");
    }
}
