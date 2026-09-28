#![allow(dead_code)]

use std::{future::Future, sync::Arc};

use quick_xml::{Reader, events::Event};
use reqwest::{Client, Method, StatusCode, header};
use thiserror::Error;
use url::Url;

use crate::auth::{AuthError, HttpAuthError, SingleFlightAuth};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalendarProvider {
    Google,
    CalDav,
}

pub const COSMIC_PASTEL_PALETTE: [&str; 8] = [
    "#7aa2f7", // Soft Blue
    "#9ece6a", // Soft Green
    "#e0af68", // Soft Yellow
    "#bb9af7", // Soft Purple
    "#f7768e", // Soft Red
    "#7dcfff", // Soft Cyan
    "#ff9e64", // Soft Orange
    "#b4f9f8", // Soft Teal
];

pub fn fallback_calendar_color(collection_key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    collection_key.hash(&mut hasher);
    let hash = hasher.finish();
    let index = (hash as usize) % COSMIC_PASTEL_PALETTE.len();
    COSMIC_PASTEL_PALETTE[index].to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredCollection {
    pub href: Url,
    pub display_name: Option<String>,
    pub color: Option<String>,
    pub ctag: Option<String>,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("invalid server URL: {0}")]
    InvalidServerUrl(#[from] url::ParseError),
    #[error("server URL must use HTTP or HTTPS")]
    UnsupportedScheme,
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("server returned HTTP status {0}")]
    HttpStatus(StatusCode),
    #[error("invalid HTTP method: {0}")]
    InvalidMethod(String),
    #[error("invalid WebDAV XML: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("authentication error: {0}")]
    Auth(#[from] HttpAuthError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryResult {
    pub principal_url: Url,
    pub calendar_home_url: Url,
    pub collections: Vec<DiscoveredCollection>,
}

impl DiscoveryResult {
    pub fn collection_hrefs(&self) -> Vec<Url> {
        self.collections.iter().map(|c| c.href.clone()).collect()
    }
}

#[derive(Clone)]
pub struct DiscoveryClient {
    http: Client,
    auth_coordinator: Option<Arc<SingleFlightAuth>>,
}

enum DiscoveryAuth<'a> {
    Bearer(&'a str),
    Basic(&'a str, &'a str),
}

impl DiscoveryClient {
    pub fn new(http: Client) -> Self {
        Self {
            http,
            auth_coordinator: None,
        }
    }

    pub fn with_auth_coordinator(http: Client, auth_coordinator: Arc<SingleFlightAuth>) -> Self {
        Self {
            http,
            auth_coordinator: Some(auth_coordinator),
        }
    }

    pub async fn discover(&self, root_url: Url) -> Result<DiscoveryResult, DiscoveryError> {
        self.discover_with_bearer(root_url, None).await
    }

    pub async fn discover_with_bearer(
        &self,
        root_url: Url,
        access_token: Option<&str>,
    ) -> Result<DiscoveryResult, DiscoveryError> {
        self.discover_with_auth(root_url, access_token.map(DiscoveryAuth::Bearer))
            .await
    }

    pub async fn discover_with_basic(
        &self,
        root_url: Url,
        username: &str,
        password: &str,
    ) -> Result<DiscoveryResult, DiscoveryError> {
        self.discover_with_auth(root_url, Some(DiscoveryAuth::Basic(username, password)))
            .await
    }

    pub async fn discover_with_bearer_retry<F, Fut>(
        &self,
        root_url: Url,
        account_id: &str,
        access_token: &str,
        refresh: F,
    ) -> Result<DiscoveryResult, DiscoveryError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, AuthError>> + Send + 'static,
    {
        if let Some(coordinator) = &self.auth_coordinator {
            let refresh = Arc::new(refresh);
            let principal_xml = self
                .propfind_with_single_flight(
                    &root_url,
                    "0",
                    account_id,
                    access_token,
                    coordinator,
                    refresh.clone(),
                )
                .await?;
            let principal_href = first_href(&principal_xml, "current-user-principal")
                .or_else(|| first_href(&principal_xml, "href"))
                .ok_or(DiscoveryError::UnsupportedScheme)?;
            let principal_url = root_url.join(&principal_href)?;

            let home_xml = self
                .propfind_with_single_flight(
                    &principal_url,
                    "0",
                    account_id,
                    access_token,
                    coordinator,
                    refresh.clone(),
                )
                .await?;
            let home_href = first_href(&home_xml, "calendar-home-set")
                .or_else(|| first_href(&home_xml, "href"))
                .ok_or(DiscoveryError::UnsupportedScheme)?;
            let calendar_home_url = principal_url.join(&home_href)?;

            let collections_xml = self
                .propfind_with_single_flight(
                    &calendar_home_url,
                    "1",
                    account_id,
                    access_token,
                    coordinator,
                    refresh,
                )
                .await?;
            let collections = parse_collections_multistatus(&collections_xml, &calendar_home_url)?;

            Ok(DiscoveryResult {
                principal_url,
                calendar_home_url,
                collections,
            })
        } else {
            self.discover_with_bearer(root_url, Some(access_token))
                .await
        }
    }

    async fn discover_with_auth(
        &self,
        root_url: Url,
        auth: Option<DiscoveryAuth<'_>>,
    ) -> Result<DiscoveryResult, DiscoveryError> {
        let principal_xml = self.propfind(&root_url, "0", auth.as_ref()).await?;
        let principal_href = first_href(&principal_xml, "current-user-principal")
            .or_else(|| first_href(&principal_xml, "href"))
            .ok_or(DiscoveryError::UnsupportedScheme)?;
        let principal_url = root_url.join(&principal_href)?;

        let home_xml = self.propfind(&principal_url, "0", auth.as_ref()).await?;
        let home_href = first_href(&home_xml, "calendar-home-set")
            .or_else(|| first_href(&home_xml, "href"))
            .ok_or(DiscoveryError::UnsupportedScheme)?;
        let calendar_home_url = principal_url.join(&home_href)?;

        let collections_xml = self
            .propfind(&calendar_home_url, "1", auth.as_ref())
            .await?;
        let collections = parse_collections_multistatus(&collections_xml, &calendar_home_url)?;

        Ok(DiscoveryResult {
            principal_url,
            calendar_home_url,
            collections,
        })
    }

    async fn propfind(
        &self,
        url: &Url,
        depth: &str,
        auth: Option<&DiscoveryAuth<'_>>,
    ) -> Result<String, DiscoveryError> {
        let method = Method::from_bytes(b"PROPFIND")
            .map_err(|error| DiscoveryError::InvalidMethod(error.to_string()))?;
        let mut request = self
            .http
            .request(method, url.clone())
            .header(header::HeaderName::from_static("depth"), depth)
            .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
            .body("<propfind xmlns=\"DAV:\"><prop><current-user-principal/><calendar-home-set/><resourcetype/><displayname/><calendar-color xmlns=\"http://apple.com/ns/ical/\"/><getctag xmlns=\"http://calendarserver.org/ns/\"/></prop></propfind>");
        if let Some(auth) = auth {
            match auth {
                DiscoveryAuth::Bearer(token) => request = request.bearer_auth(token),
                DiscoveryAuth::Basic(username, password) => {
                    request = request.basic_auth(username, Some(password));
                }
            }
        }
        let response = request.send().await?;
        if response.status() != StatusCode::MULTI_STATUS {
            return Err(DiscoveryError::HttpStatus(response.status()));
        }
        Ok(response.text().await?)
    }

    async fn propfind_with_single_flight<F, Fut>(
        &self,
        url: &Url,
        depth: &str,
        account_id: &str,
        initial_token: &str,
        coordinator: &SingleFlightAuth,
        refresh: Arc<F>,
    ) -> Result<String, DiscoveryError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, AuthError>> + Send + 'static,
    {
        let depth_str = depth.to_string();
        let target_url = url.clone();
        let client = self.http.clone();

        let response = coordinator
            .execute_authed_request(
                account_id,
                Some(initial_token),
                move || {
                    let r = refresh.clone();
                    async move { (r)().await }
                },
                move |token| {
                    let method = Method::from_bytes(b"PROPFIND").unwrap();
                    let mut req = client
                        .request(method, target_url.clone())
                        .header(header::HeaderName::from_static("depth"), &depth_str)
                        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
                        .body("<propfind xmlns=\"DAV:\"><prop><current-user-principal/><calendar-home-set/><resourcetype/><displayname/><calendar-color xmlns=\"http://apple.com/ns/ical/\"/><getctag xmlns=\"http://calendarserver.org/ns/\"/></prop></propfind>");
                    if let Some(t) = token {
                        req = req.bearer_auth(t);
                    }
                    req
                },
            )
            .await?;

        if response.status() != StatusCode::MULTI_STATUS {
            return Err(DiscoveryError::HttpStatus(response.status()));
        }
        Ok(response.text().await?)
    }
}

pub fn parse_collections_multistatus(
    xml: &str,
    calendar_home_url: &Url,
) -> Result<Vec<DiscoveredCollection>, DiscoveryError> {
    let mut reader = Reader::from_str(xml);
    let mut collections = Vec::new();

    let mut in_response = false;
    let mut current_href: Option<String> = None;
    let mut in_href = false;
    let mut in_resourcetype = false;
    let mut is_calendar = false;
    let mut in_displayname = false;
    let mut current_displayname: Option<String> = None;
    let mut in_color = false;
    let mut current_color: Option<String> = None;
    let mut in_ctag = false;
    let mut current_ctag: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"response" => {
                    in_response = true;
                    current_href = None;
                    is_calendar = false;
                    current_displayname = None;
                    current_color = None;
                    current_ctag = None;
                }
                b"href" if in_response => in_href = true,
                b"resourcetype" if in_response => in_resourcetype = true,
                b"calendar" if in_resourcetype => is_calendar = true,
                b"displayname" if in_response => in_displayname = true,
                b"calendar-color" if in_response => in_color = true,
                b"getctag" | b"ctag" if in_response => in_ctag = true,
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if in_resourcetype && e.local_name().as_ref() == b"calendar" {
                    is_calendar = true;
                }
            }
            Ok(Event::Text(t)) => {
                if in_href && current_href.is_none() {
                    current_href = Some(String::from_utf8_lossy(t.as_ref()).trim().to_string());
                } else if in_displayname {
                    current_displayname =
                        Some(String::from_utf8_lossy(t.as_ref()).trim().to_string());
                } else if in_color {
                    current_color = Some(String::from_utf8_lossy(t.as_ref()).trim().to_string());
                } else if in_ctag {
                    current_ctag = Some(String::from_utf8_lossy(t.as_ref()).trim().to_string());
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"href" => in_href = false,
                b"resourcetype" => in_resourcetype = false,
                b"displayname" => in_displayname = false,
                b"calendar-color" => in_color = false,
                b"getctag" | b"ctag" => in_ctag = false,
                b"response" => {
                    in_response = false;
                    if is_calendar
                        && let Some(href_str) = current_href.take()
                        && let Ok(resolved_url) = calendar_home_url.join(&href_str)
                    {
                        let color = current_color
                            .take()
                            .filter(|c| !c.is_empty())
                            .unwrap_or_else(|| fallback_calendar_color(resolved_url.as_str()));
                        collections.push(DiscoveredCollection {
                            href: resolved_url,
                            display_name: current_displayname.take().filter(|d| !d.is_empty()),
                            color: Some(color),
                            ctag: current_ctag.take().filter(|c| !c.is_empty()),
                        });
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "Malformed XML encountered while parsing collections"
                );
                break;
            }
        }
    }

    Ok(collections)
}

fn first_href(xml: &str, parent: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut in_parent = false;
    let mut in_href = false;
    let mut value = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if element.local_name().as_ref() == parent.as_bytes() => {
                in_parent = true;
            }
            Ok(Event::Start(element)) if in_parent && element.local_name().as_ref() == b"href" => {
                in_href = true;
            }
            Ok(Event::Text(text)) if in_href => {
                value = Some(String::from_utf8_lossy(text.as_ref()).trim().to_string());
            }
            Ok(Event::End(element)) if element.local_name().as_ref() == b"href" => {
                in_href = false;
            }
            Ok(Event::End(element)) if element.local_name().as_ref() == parent.as_bytes() => {
                in_parent = false;
                if value.is_some() {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    ?error,
                    parent,
                    "Malformed XML encountered while looking for WebDAV href"
                );
                break;
            }
        }
    }
    value
}

pub fn calendar_root_url(
    provider: CalendarProvider,
    server_url: Option<&str>,
) -> Result<Url, DiscoveryError> {
    let raw_url = match provider {
        CalendarProvider::Google => "https://apidata.googleusercontent.com/caldav/v2/",
        CalendarProvider::CalDav => server_url.ok_or(DiscoveryError::UnsupportedScheme)?,
    };
    let mut url = Url::parse(raw_url)?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(DiscoveryError::UnsupportedScheme);
    }
    if matches!(provider, CalendarProvider::CalDav) && !url.path().ends_with("/remote.php/dav/") {
        let path = url.path().trim_end_matches('/');
        url.set_path(&format!("{path}/remote.php/dav/"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_uses_caldav_root() {
        assert_eq!(
            calendar_root_url(CalendarProvider::Google, None)
                .unwrap()
                .as_str(),
            "https://apidata.googleusercontent.com/caldav/v2/"
        );
    }

    #[test]
    fn nextcloud_server_url_gets_dav_suffix() {
        assert_eq!(
            calendar_root_url(
                CalendarProvider::CalDav,
                Some("https://calendar.example.test")
            )
            .unwrap()
            .as_str(),
            "https://calendar.example.test/remote.php/dav/"
        );
    }

    #[test]
    fn unsupported_server_scheme_is_rejected() {
        assert!(matches!(
            calendar_root_url(CalendarProvider::CalDav, Some("file:///calendar")),
            Err(DiscoveryError::UnsupportedScheme)
        ));
    }

    #[test]
    fn parse_collections_filters_non_calendars_and_extracts_metadata() {
        let xml = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:apple="http://apple.com/ns/ical/" xmlns:cs="http://calendarserver.org/ns/">
            <!-- Home collection itself - NOT a calendar -->
            <d:response>
                <d:href>/calendars/user/</d:href>
                <d:propstat>
                    <d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>
                </d:propstat>
            </d:response>
            <!-- Calendar A with full metadata -->
            <d:response>
                <d:href>/calendars/user/work/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
                        <d:displayname>Work Calendar</d:displayname>
                        <apple:calendar-color>#ff0000ff</apple:calendar-color>
                        <cs:getctag>"ctag-work-1"</cs:getctag>
                    </d:prop>
                </d:propstat>
            </d:response>
            <!-- Calendar B without color - gets fallback -->
            <d:response>
                <d:href>/calendars/user/personal/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
                        <d:displayname>Personal Calendar</d:displayname>
                        <cs:getctag>"ctag-pers-1"</cs:getctag>
                    </d:prop>
                </d:propstat>
            </d:response>
            <!-- Addressbook collection - NOT a calendar -->
            <d:response>
                <d:href>/addressbooks/user/</d:href>
                <d:propstat>
                    <d:prop><d:resourcetype><d:collection/><card:addressbook xmlns:card="urn:ietf:params:xml:ns:carddav"/></d:resourcetype></d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let home_url = Url::parse("https://cal.example.com/calendars/user/").unwrap();
        let cols = parse_collections_multistatus(xml, &home_url).unwrap();

        assert_eq!(cols.len(), 2);
        assert_eq!(
            cols[0].href.as_str(),
            "https://cal.example.com/calendars/user/work/"
        );
        assert_eq!(cols[0].display_name.as_deref(), Some("Work Calendar"));
        assert_eq!(cols[0].color.as_deref(), Some("#ff0000ff"));
        assert_eq!(cols[0].ctag.as_deref(), Some("\"ctag-work-1\""));

        assert_eq!(
            cols[1].href.as_str(),
            "https://cal.example.com/calendars/user/personal/"
        );
        assert_eq!(cols[1].display_name.as_deref(), Some("Personal Calendar"));
        assert!(cols[1].color.is_some());
        assert!(COSMIC_PASTEL_PALETTE.contains(&cols[1].color.as_ref().unwrap().as_str()));
    }

    #[test]
    fn malformed_xml_does_not_panic_and_recovers_gracefully() {
        let malformed = "<multistatus><response><href>/unclosed";
        assert_eq!(first_href(malformed, "current-user-principal"), None);

        let home_url = Url::parse("https://cal.example.com/calendars/user/").unwrap();
        let cols = parse_collections_multistatus(malformed, &home_url).unwrap();
        assert!(cols.is_empty());
    }

    #[test]
    fn principal_href_is_nested_under_requested_property() {
        let xml = r#"<propstat xmlns="DAV:"><current-user-principal><href>/principals/u/</href></current-user-principal><href>/wrong/</href></propstat>"#;
        assert_eq!(
            first_href(xml, "current-user-principal"),
            Some("/principals/u/".to_string())
        );
    }

    #[tokio::test]
    async fn discovery_runs_the_three_propfind_steps() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let responses = [
                r#"<multistatus xmlns="DAV:"><response><propstat><prop><current-user-principal><href>/principals/user/</href></current-user-principal></prop></propstat></response></multistatus>"#,
                r#"<multistatus xmlns="DAV:"><response><propstat><prop><calendar-home-set><href>/calendars/user/</href></calendar-home-set></prop></propstat></response></multistatus>"#,
                r#"<multistatus xmlns="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
                    <response><href>/calendars/user/work/</href><propstat><prop><resourcetype><collection/><c:calendar/></resourcetype><displayname>Work</displayname></prop></propstat></response>
                    <response><href>/calendars/user/personal/</href><propstat><prop><resourcetype><collection/><c:calendar/></resourcetype><displayname>Personal</displayname></prop></propstat></response>
                </multistatus>"#,
            ];

            for body in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 4096];
                let length = socket.read(&mut request).await.unwrap();
                assert!(String::from_utf8_lossy(&request[..length]).starts_with("PROPFIND"));
                let response = format!(
                    "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let root = Url::parse(&format!("http://{address}/")).unwrap();
        let result = DiscoveryClient::new(Client::new())
            .discover(root)
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(
            result.principal_url.as_str(),
            &format!("http://{address}/principals/user/")
        );
        assert_eq!(
            result.calendar_home_url.as_str(),
            &format!("http://{address}/calendars/user/")
        );
        assert_eq!(result.collections.len(), 2);
        assert_eq!(result.collection_hrefs().len(), 2);
    }
}
