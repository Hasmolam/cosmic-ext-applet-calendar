use std::{collections::HashMap, future::Future, sync::Arc};

use thiserror::Error;
use tokio::sync::{Mutex, OnceCell};

type RefreshCell = Arc<OnceCell<Result<String, AuthError>>>;

#[derive(Clone)]
struct RefreshEntry {
    failed_token: Option<String>,
    cell: RefreshCell,
}

type RefreshEntries = Arc<Mutex<HashMap<String, RefreshEntry>>>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("token refresh failed: {0}")]
pub struct AuthError(pub String);

#[derive(Debug, Error)]
pub enum HttpAuthError {
    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Token refresh error: {0}")]
    Refresh(#[from] AuthError),
}

#[derive(Clone, Default)]
pub struct SingleFlightAuth {
    entries: RefreshEntries,
}

impl SingleFlightAuth {
    #[allow(dead_code)]
    pub async fn refresh_once<F, Fut>(
        &self,
        account_id: &str,
        refresh: F,
    ) -> Result<String, AuthError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, AuthError>>,
    {
        self.refresh_on_unauthorized(account_id, None, refresh)
            .await
    }

    pub async fn refresh_on_unauthorized<F, Fut>(
        &self,
        account_id: &str,
        failed_token: Option<&str>,
        refresh: F,
    ) -> Result<String, AuthError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, AuthError>>,
    {
        let cell = {
            let mut entries = self.entries.lock().await;
            if let Some(entry) = entries.get(account_id) {
                if entry.failed_token.as_deref() == failed_token {
                    entry.cell.clone()
                } else {
                    let cell = Arc::new(OnceCell::new());
                    entries.insert(
                        account_id.to_string(),
                        RefreshEntry {
                            failed_token: failed_token.map(str::to_string),
                            cell: cell.clone(),
                        },
                    );
                    cell
                }
            } else {
                let cell = Arc::new(OnceCell::new());
                entries.insert(
                    account_id.to_string(),
                    RefreshEntry {
                        failed_token: failed_token.map(str::to_string),
                        cell: cell.clone(),
                    },
                );
                cell
            }
        };
        cell.get_or_init(refresh).await.clone()
    }

    pub async fn execute_authed_request<F, Fut, B>(
        &self,
        account_id: &str,
        initial_token: Option<&str>,
        refresh: F,
        build_request: B,
    ) -> Result<reqwest::Response, HttpAuthError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<String, AuthError>>,
        B: Fn(Option<&str>) -> reqwest::RequestBuilder,
    {
        let response = build_request(initial_token)
            .send()
            .await
            .map_err(HttpAuthError::Http)?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            tracing::warn!(
                account_id,
                "Received 401 Unauthorized; coordinating single-flight token refresh"
            );
            let new_token = self
                .refresh_on_unauthorized(account_id, initial_token, refresh)
                .await
                .map_err(HttpAuthError::Refresh)?;
            let retry_response = build_request(Some(&new_token))
                .send()
                .await
                .map_err(HttpAuthError::Http)?;
            Ok(retry_response)
        } else {
            Ok(response)
        }
    }

    #[allow(dead_code)]
    pub async fn invalidate(&self, account_id: &str) {
        self.entries.lock().await.remove(account_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn concurrent_refreshes_share_one_result() {
        let auth = SingleFlightAuth::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..8 {
            let auth = auth.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                auth.refresh_once("account-a", || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, AuthError>("token-a".to_string())
                })
                .await
            }));
        }

        for task in tasks {
            assert_eq!(task.await.unwrap().unwrap(), "token-a");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalidation_allows_a_new_refresh() {
        let auth = SingleFlightAuth::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let refresh = || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, AuthError>("token".to_string())
            }
        };

        auth.refresh_once("account-a", refresh).await.unwrap();
        auth.invalidate("account-a").await;
        auth.refresh_once("account-a", || async {
            Ok::<_, AuthError>("token-2".to_string())
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_401_single_flight_refresh_and_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            for _ in 0..8 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 1024];
                let n = socket.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);

                if req.contains("Bearer bad-token") {
                    let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
                    socket.write_all(resp.as_bytes()).await.unwrap();
                } else if req.contains("Bearer good-token") {
                    let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
                    socket.write_all(resp.as_bytes()).await.unwrap();
                } else {
                    let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
                    socket.write_all(resp.as_bytes()).await.unwrap();
                }
            }
        });

        let auth = Arc::new(SingleFlightAuth::default());
        let refresh_count = Arc::new(AtomicUsize::new(0));
        let http = reqwest::Client::new();
        let url = format!("http://{addr}/test");

        let mut tasks = Vec::new();
        for _ in 0..4 {
            let auth = auth.clone();
            let refresh_count = refresh_count.clone();
            let http = http.clone();
            let url = url.clone();

            tasks.push(tokio::spawn(async move {
                let refresh = {
                    let refresh_count = refresh_count.clone();
                    move || {
                        let refresh_count = refresh_count.clone();
                        async move {
                            refresh_count.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, AuthError>("good-token".to_string())
                        }
                    }
                };

                auth.execute_authed_request("account-test", Some("bad-token"), refresh, |token| {
                    let mut req = http.get(&url);
                    if let Some(t) = token {
                        req = req.bearer_auth(t);
                    }
                    req
                })
                .await
            }));
        }

        for task in tasks {
            let res = task.await.unwrap().unwrap();
            assert_eq!(res.status(), reqwest::StatusCode::OK);
        }

        server.await.unwrap();
        assert_eq!(refresh_count.load(Ordering::SeqCst), 1);
    }
}
