use quick_xml::{Reader, events::Event};
use reqwest::{Client, Method, StatusCode, header};
use std::sync::Arc;
use thiserror::Error;
use url::Url;

use crate::auth::{AuthError, HttpAuthError, SingleFlightAuth};

#[derive(Debug, Error)]
pub enum CalDavSyncError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("server returned HTTP status {0}")]
    HttpStatus(StatusCode),
    #[error("sync token expired (HTTP 410 Gone / invalid-sync-token)")]
    SyncTokenExpired,
    #[error("XML parsing error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("authentication error: {0}")]
    Auth(#[from] HttpAuthError),
    #[error("refresh error: {0}")]
    Refresh(#[from] AuthError),
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncAuth {
    Bearer(String),
    Basic(String, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncItemChange {
    Updated { href: String, etag: Option<String> },
    Deleted { href: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncCollectionResult {
    pub new_sync_token: String,
    pub changes: Vec<SyncItemChange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultigetItem {
    pub href: String,
    pub etag: String,
    pub ics_data: String,
}

#[derive(Clone)]
pub struct CalDavClient {
    http: Client,
    auth_coordinator: Option<Arc<SingleFlightAuth>>,
}

impl CalDavClient {
    #[allow(dead_code)]
    pub fn new(http: Client) -> Self {
        Self {
            http,
            auth_coordinator: None,
        }
    }

    pub fn with_auth_coordinator(http: Client, auth: Arc<SingleFlightAuth>) -> Self {
        Self {
            http,
            auth_coordinator: Some(auth),
        }
    }

    pub async fn sync_collection<F, Fut>(
        &self,
        collection_url: &Url,
        account_id: &str,
        auth: &SyncAuth,
        sync_token: Option<&str>,
        sync_horizon_epoch: Option<i64>,
        token_refresh: F,
    ) -> Result<SyncCollectionResult, CalDavSyncError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, AuthError>> + Send + 'static,
    {
        let body = if let Some(token) = sync_token {
            format!(
                r#"<?xml version="1.0" encoding="utf-8" ?>
<D:sync-collection xmlns:D="DAV:">
  <D:sync-token>{token}</D:sync-token>
  <D:sync-level>1</D:sync-level>
  <D:prop>
    <D:getetag/>
  </D:prop>
</D:sync-collection>"#
            )
        } else if let Some(horizon) = sync_horizon_epoch {
            // Initial sync with Sync Horizon (PLAN.md 5.7)
            let horizon_dt = chrono::DateTime::from_timestamp(horizon, 0).unwrap_or_default();
            let horizon_str = horizon_dt.format("%Y%m%dT000000Z").to_string();
            format!(
                r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{horizon_str}"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#
            )
        } else {
            // Initial sync: fetch all hrefs and etags
            r#"<?xml version="1.0" encoding="utf-8" ?>
<D:sync-collection xmlns:D="DAV:">
  <D:sync-token/>
  <D:sync-level>1</D:sync-level>
  <D:prop>
    <D:getetag/>
  </D:prop>
</D:sync-collection>"#
                .to_string()
        };

        let response = self
            .send_authed_request(
                collection_url,
                Method::from_bytes(b"REPORT").unwrap(),
                body,
                account_id,
                auth,
                token_refresh,
            )
            .await?;

        if response.status() == StatusCode::GONE {
            return Err(CalDavSyncError::SyncTokenExpired);
        }

        if !response.status().is_success() && response.status() != StatusCode::MULTI_STATUS {
            return Err(CalDavSyncError::HttpStatus(response.status()));
        }

        let xml = response.text().await?;
        if xml.contains("valid-sync-token") {
            return Err(CalDavSyncError::SyncTokenExpired);
        }

        parse_sync_collection_response(&xml)
    }

    pub async fn multiget<F, Fut>(
        &self,
        collection_url: &Url,
        account_id: &str,
        auth: &SyncAuth,
        hrefs: &[String],
        token_refresh: Arc<F>,
    ) -> Result<Vec<MultigetItem>, CalDavSyncError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, AuthError>> + Send + 'static,
    {
        if hrefs.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_items = Vec::new();

        // Chunked multiget: max 50 hrefs per request (PLAN.md 5.6)
        for chunk in hrefs.chunks(50) {
            let mut href_elements = String::new();
            for href in chunk {
                href_elements.push_str(&format!("<D:href>{href}</D:href>\n"));
            }

            let body = format!(
                r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
  {href_elements}
</C:calendar-multiget>"#
            );

            let refresh_clone = token_refresh.clone();
            let response = self
                .send_authed_request(
                    collection_url,
                    Method::from_bytes(b"REPORT").unwrap(),
                    body,
                    account_id,
                    auth,
                    move || refresh_clone(),
                )
                .await?;

            if !response.status().is_success() && response.status() != StatusCode::MULTI_STATUS {
                return Err(CalDavSyncError::HttpStatus(response.status()));
            }

            let xml = response.text().await?;
            let items = parse_multiget_response(&xml)?;
            all_items.extend(items);
        }

        Ok(all_items)
    }

    async fn send_authed_request<F, Fut>(
        &self,
        url: &Url,
        method: Method,
        body: String,
        account_id: &str,
        auth: &SyncAuth,
        token_refresh: F,
    ) -> Result<reqwest::Response, CalDavSyncError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, AuthError>> + Send + 'static,
    {
        match auth {
            SyncAuth::Bearer(token) => {
                let http = self.http.clone();
                let url = url.clone();
                let method = method.clone();
                let body = body.clone();

                if let Some(coordinator) = &self.auth_coordinator {
                    let coordinator = coordinator.clone();
                    let response = coordinator
                        .execute_authed_request(
                            account_id,
                            Some(token.as_str()),
                            token_refresh,
                            move |t| {
                                let mut req = http
                                    .request(method.clone(), url.clone())
                                    .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
                                    .header("Depth", "1")
                                    .body(body.clone());
                                if let Some(t) = t {
                                    req = req.bearer_auth(t);
                                }
                                req
                            },
                        )
                        .await?;
                    Ok(response)
                } else {
                    let req = self
                        .http
                        .request(method, url)
                        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
                        .header("Depth", "1")
                        .bearer_auth(token)
                        .body(body);
                    let resp = req.send().await?;
                    Ok(resp)
                }
            }
            SyncAuth::Basic(username, password) => {
                let req = self
                    .http
                    .request(method, url.clone())
                    .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
                    .header("Depth", "1")
                    .basic_auth(username, Some(password))
                    .body(body);
                let resp = req.send().await?;
                Ok(resp)
            }
        }
    }
}

pub fn parse_sync_collection_response(xml: &str) -> Result<SyncCollectionResult, CalDavSyncError> {
    let mut reader = Reader::from_str(xml);
    let mut in_response = false;
    let mut in_href = false;
    let mut in_status = false;
    let mut in_etag = false;
    let mut in_sync_token = false;

    let mut current_href: Option<String> = None;
    let mut current_status: Option<String> = None;
    let mut current_etag: Option<String> = None;
    let mut sync_token = String::new();

    let mut changes = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"response" => {
                    in_response = true;
                    current_href = None;
                    current_status = None;
                    current_etag = None;
                }
                b"href" => in_href = true,
                b"status" => in_status = true,
                b"getetag" => in_etag = true,
                b"sync-token" => in_sync_token = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                let text = String::from_utf8_lossy(t.as_ref()).trim().to_string();
                if in_sync_token {
                    sync_token = text;
                } else if in_response {
                    if in_href && current_href.is_none() {
                        current_href = Some(text);
                    } else if in_status && current_status.is_none() {
                        current_status = Some(text);
                    } else if in_etag && current_etag.is_none() {
                        current_etag = Some(text.trim_matches('"').to_string());
                    }
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"response" => {
                    in_response = false;
                    if let Some(href) = current_href.take() {
                        let status = current_status.take().unwrap_or_default();
                        if status.contains("404") {
                            changes.push(SyncItemChange::Deleted { href });
                        } else {
                            changes.push(SyncItemChange::Updated {
                                href,
                                etag: current_etag.take(),
                            });
                        }
                    }
                }
                b"href" => in_href = false,
                b"status" => in_status = false,
                b"getetag" => in_etag = false,
                b"sync-token" => in_sync_token = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(CalDavSyncError::Xml(e)),
        }
    }

    Ok(SyncCollectionResult {
        new_sync_token: sync_token,
        changes,
    })
}

pub fn parse_multiget_response(xml: &str) -> Result<Vec<MultigetItem>, CalDavSyncError> {
    let mut reader = Reader::from_str(xml);
    let mut in_response = false;
    let mut in_href = false;
    let mut in_etag = false;
    let mut in_calendar_data = false;

    let mut current_href: Option<String> = None;
    let mut current_etag: Option<String> = None;
    let mut current_ics = String::new();

    let mut items = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"response" => {
                    in_response = true;
                    current_href = None;
                    current_etag = None;
                    current_ics.clear();
                }
                b"href" => in_href = true,
                b"getetag" => in_etag = true,
                b"calendar-data" => in_calendar_data = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_response {
                    if in_href && current_href.is_none() {
                        current_href = Some(String::from_utf8_lossy(t.as_ref()).trim().to_string());
                    } else if in_etag && current_etag.is_none() {
                        current_etag = Some(
                            String::from_utf8_lossy(t.as_ref())
                                .trim()
                                .trim_matches('"')
                                .to_string(),
                        );
                    } else if in_calendar_data {
                        current_ics.push_str(&String::from_utf8_lossy(t.as_ref()));
                    }
                }
            }
            Ok(Event::CData(cd)) => {
                if in_response && in_calendar_data {
                    current_ics.push_str(&String::from_utf8_lossy(cd.as_ref()));
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"response" => {
                    in_response = false;
                    if let Some(href) = current_href.take()
                        && !current_ics.is_empty()
                    {
                        items.push(MultigetItem {
                            href,
                            etag: current_etag.take().unwrap_or_default(),
                            ics_data: current_ics.clone(),
                        });
                    }
                }
                b"href" => in_href = false,
                b"getetag" => in_etag = false,
                b"calendar-data" => in_calendar_data = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(CalDavSyncError::Xml(e)),
        }
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sync_collection_with_updates_deletions_and_token() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/cal/event1.ics</D:href>
    <D:propstat>
      <D:prop><D:getetag>"etag-123"</D:getetag></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/cal/deleted.ics</D:href>
    <D:status>HTTP/1.1 404 Not Found</D:status>
  </D:response>
  <D:sync-token>http://example.com/sync/token-abc</D:sync-token>
</D:multistatus>"#;

        let result = parse_sync_collection_response(xml).unwrap();
        assert_eq!(result.new_sync_token, "http://example.com/sync/token-abc");
        assert_eq!(result.changes.len(), 2);

        assert_eq!(
            result.changes[0],
            SyncItemChange::Updated {
                href: "/cal/event1.ics".to_string(),
                etag: Some("etag-123".to_string()),
            }
        );

        assert_eq!(
            result.changes[1],
            SyncItemChange::Deleted {
                href: "/cal/deleted.ics".to_string(),
            }
        );
    }

    #[test]
    fn parses_zero_changes_preserves_new_sync_token() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:multistatus xmlns:D="DAV:">
  <D:sync-token>token-after-zero-changes</D:sync-token>
</D:multistatus>"#;

        let result = parse_sync_collection_response(xml).unwrap();
        assert_eq!(result.new_sync_token, "token-after-zero-changes");
        assert!(result.changes.is_empty());
    }

    #[test]
    fn parses_multiget_response_items() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/cal/event1.ics</D:href>
    <D:propstat>
      <D:prop>
        <D:getetag>"etag-1"</D:getetag>
        <C:calendar-data><![CDATA[BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:ev-1
SUMMARY:Multiget Test
END:VEVENT
END:VCALENDAR]]></C:calendar-data>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let items = parse_multiget_response(xml).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].href, "/cal/event1.ics");
        assert_eq!(items[0].etag, "etag-1");
        assert!(items[0].ics_data.contains("SUMMARY:Multiget Test"));
    }
}
