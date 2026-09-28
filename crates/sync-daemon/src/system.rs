//! System event listeners for network state, timezone changes, and
//! suspend/resume cycles.
//!
//! All three listeners use raw `zbus` D-Bus calls so the crate stays on
//! `zbus 4` without pulling in `ashpd` or `logind-zbus` (both require zbus 5+).

use std::ops::Deref;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures_util::StreamExt;
use zbus::{Connection, MatchRule, MessageStream};

/// Shared network-availability flag.
#[derive(Clone)]
pub struct NetworkState {
    /// `false` when in airplane mode or the network is completely unavailable.
    pub available: Arc<AtomicBool>,
    /// `true` when the active connection is metered (mobile hotspot, etc.).
    pub metered: Arc<AtomicBool>,
}

impl Default for NetworkState {
    fn default() -> Self {
        Self {
            // Optimistic default: assume network is up until told otherwise.
            available: Arc::new(AtomicBool::new(true)),
            metered: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl NetworkState {
    /// Returns `true` when periodic sync should run.
    pub fn sync_allowed(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// Returns the sync interval in seconds.  15 min normally, 120 min on a
    /// metered connection.
    pub fn sync_interval_secs(&self) -> u64 {
        if self.metered.load(Ordering::Relaxed) {
            120 * 60
        } else {
            15 * 60
        }
    }
}

// ---------------------------------------------------------------------------
// Network listener
// ---------------------------------------------------------------------------

/// Watches `org.freedesktop.NetworkManager` property changes and keeps
/// [`NetworkState`] up to date.
///
/// * `Connectivity == 4` (FULL) → available = true
/// * `Connectivity != 4`       → available = false; sync paused
/// * `Metered == 1|2`          → metered = true; cadence extended to 2 h
///
/// On the unavailable → available transition, `on_reconnect` is called once
/// so the caller can trigger an immediate sync without waiting for the timer.
pub async fn watch_network<F, Fut>(state: NetworkState, on_reconnect: F) -> zbus::Result<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let conn = Connection::system().await?;

    // Read initial NM state if the daemon is present.
    let initial = read_nm_properties(&conn).await;
    if let Some((c, m)) = initial {
        state.available.store(c == 4, Ordering::Relaxed);
        state.metered.store(m == 1 || m == 2, Ordering::Relaxed);
    }

    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path("/org/freedesktop/NetworkManager")?
        .build();

    let mut stream = MessageStream::for_match_rule(rule, &conn, Some(64)).await?;

    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(?e, "NetworkManager signal stream error");
                continue;
            }
        };

        let body: zbus::Result<(
            String,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            Vec<String>,
        )> = msg.body().deserialize();

        let (iface, changed, _) = match body {
            Ok(b) => b,
            Err(_) => continue,
        };

        if iface != "org.freedesktop.NetworkManager" {
            continue;
        }

        let was_available = state.available.load(Ordering::Relaxed);

        if let Some(v) = changed.get("Connectivity")
            && let Ok(c) = u32::try_from(v)
        {
            let now_available = c == 4;
            state.available.store(now_available, Ordering::Relaxed);
            if now_available {
                tracing::info!("Network connectivity restored (full)");
            } else {
                tracing::info!(
                    connectivity = c,
                    "Network connectivity degraded — sync paused"
                );
            }
            if !was_available && now_available {
                on_reconnect().await;
            }
        }

        if let Some(v) = changed.get("Metered")
            && let Ok(m) = u32::try_from(v)
        {
            // NM: 0=unknown 1=yes 2=guessed_yes 3=no 4=guessed_no
            let is_metered = m == 1 || m == 2;
            state.metered.store(is_metered, Ordering::Relaxed);
            if is_metered {
                tracing::info!("Metered network — sync cadence extended to 2 h");
            } else {
                tracing::info!("Unmetered network — standard 15 min sync cadence");
            }
        }
    }

    Ok(())
}

async fn read_nm_properties(conn: &Connection) -> Option<(u32, u32)> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.NetworkManager"),
            "/org/freedesktop/NetworkManager",
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &("org.freedesktop.NetworkManager",),
        )
        .await
        .ok()?;

    let props: std::collections::HashMap<String, zbus::zvariant::OwnedValue> =
        reply.body().deserialize().ok()?;

    let connectivity = props
        .get("Connectivity")
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let metered = props
        .get("Metered")
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    Some((connectivity, metered))
}

// ---------------------------------------------------------------------------
// Timezone listener
// ---------------------------------------------------------------------------

/// Watches `org.freedesktop.timedate1` `PropertiesChanged` and calls
/// `on_tz_change` with the new IANA timezone string whenever the system
/// timezone is updated (e.g. user travels and clock is adjusted).
pub async fn watch_timezone<F, Fut>(on_tz_change: F) -> zbus::Result<()>
where
    F: Fn(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let conn = Connection::system().await?;

    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path("/org/freedesktop/timedate1")?
        .build();

    let mut stream = MessageStream::for_match_rule(rule, &conn, Some(16)).await?;

    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(?e, "timedate1 signal stream error");
                continue;
            }
        };

        let body: zbus::Result<(
            String,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            Vec<String>,
        )> = msg.body().deserialize();

        let (iface, changed, _) = match body {
            Ok(b) => b,
            Err(_) => continue,
        };

        if iface != "org.freedesktop.timedate1" {
            continue;
        }

        if let Some(v) = changed.get("Timezone") {
            // OwnedValue does not implement TryFrom<&OwnedValue> for String;
            // match on the inner Value variant instead.
            if let zbus::zvariant::Value::Str(tz) = v.deref() {
                let tz = tz.to_string();
                tracing::info!(timezone = %tz, "System timezone changed — emitting events_changed");
                on_tz_change(tz).await;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Sleep / wake listener
// ---------------------------------------------------------------------------

/// Watches `org.freedesktop.login1.Manager` `PrepareForSleep(bool)`.
///
/// * `true`  → machine about to suspend — no action.
/// * `false` → machine resumed — `on_wake` is called to reset the periodic
///   sync timer and trigger an immediate background sync.
pub async fn watch_sleep<F, Fut>(on_wake: F) -> zbus::Result<()>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let conn = Connection::system().await?;

    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.login1.Manager")?
        .member("PrepareForSleep")?
        .path("/org/freedesktop/login1")?
        .build();

    let mut stream = MessageStream::for_match_rule(rule, &conn, Some(16)).await?;

    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(?e, "login1 signal stream error");
                continue;
            }
        };

        let going_to_sleep: zbus::Result<(bool,)> = msg.body().deserialize();
        match going_to_sleep {
            Ok((true,)) => {
                tracing::debug!("System preparing for sleep");
            }
            Ok((false,)) => {
                tracing::info!("System resumed from sleep — triggering background sync");
                on_wake().await;
            }
            Err(e) => {
                tracing::debug!(?e, "PrepareForSleep body parse error");
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn network_state_defaults_available_not_metered() {
        let state = NetworkState::default();
        assert!(state.available.load(Ordering::Relaxed));
        assert!(!state.metered.load(Ordering::Relaxed));
    }

    #[test]
    fn sync_allowed_reflects_available_flag() {
        let state = NetworkState::default();
        assert!(state.sync_allowed());
        state.available.store(false, Ordering::Relaxed);
        assert!(!state.sync_allowed());
    }

    #[test]
    fn sync_interval_normal_vs_metered() {
        let state = NetworkState::default();
        assert_eq!(state.sync_interval_secs(), 15 * 60);
        state.metered.store(true, Ordering::Relaxed);
        assert_eq!(state.sync_interval_secs(), 120 * 60);
    }
}
