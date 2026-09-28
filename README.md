# COSMIC Calendar Applet & Sync Daemon

A calendar and agenda applet with a native Rust background synchronization daemon for the Pop!_OS COSMIC Desktop.

Replaces the legacy Evolution Data Server (EDS) and GNOME Online Accounts (GOA) stack with a pure-Rust CalDAV synchronization engine (`cosmic-calendar-sync`), local SQLite storage with WAL mode, and a lightweight D-Bus client (`calendar-client`).

---

## Architecture Overview

```text
cosmic-ext-applet-calendar (libcosmic UI)
       │
       ▼
CompositeBackend
  ├── LocalIcsBackend (~/.local/share/calendars/*.ics, /usr/share/calendar/*.ics)
  └── CalendarDbusBackend (via crates/calendar-client)
             │
             ▼ D-Bus: dev.cosmic.Calendar (/dev/cosmic/Calendar)
cosmic-calendar-sync (crates/sync-daemon)
  ├── Accounts Client (dev.edfloreshz.Accounts over session bus)
  │     ├── Google OAuth2 (strictly calendar.readonly)
  │     └── Secret Service (oo7, Nextcloud/CalDAV passwords)
  ├── CalDAV Sync Engine (RFC 4791, RFC 6578 sync-collection, multiget)
  ├── SQLite Storage (~/.local/share/cosmic-calendar/calendar.db, WAL, foreign keys)
  ├── RRULE Expansion & Exception Handling (crates/calendar-core)
  └── System Listeners (NetworkManager, timedate1, login1 sleep/wake)
```

### Workspace Crates

| Crate | License | Path | Description |
|---|---|---|---|
| `calendar-client` | MPL-2.0 | `crates/calendar-client` | Leaf client library for `dev.cosmic.Calendar` over zbus. Zero network or database dependencies. |
| `calendar-core` | MPL-2.0 | `crates/calendar-core` | SQLite schema and migrations, iCalendar parser, RRULE virtual instance expansion, meeting link extraction. |
| `cosmic-calendar-sync` | GPL-3.0-only | `crates/sync-daemon` | Headless daemon managing CalDAV sync, accounts lifecycle, secrets, and system power/network states. |
| `cosmic-ext-applet-calendar` | GPL-3.0-only | `.` | libcosmic panel applet and drop-in clock replacement. |

---

## Features

- **Pure Rust Stack:** Zero runtime dependencies on GNOME, Evolution Data Server, or C libraries.
- **CalDAV Sync Engine:** Implements RFC 4791 and RFC 6578 incremental collection synchronization with chunked multiget downloads.
- **Strictly Read-Only Google Scope:** Requests `https://www.googleapis.com/auth/calendar.readonly`, eliminating remote modification risk.
- **Desktop Consent UI Attribution:** Ships `dev.cosmic.Calendar.desktop` with `X-CosmicAppId` for Wayland-native consent prompts without Polkit elevation.
- **SQLite WAL Storage:** Events and metadata are indexed locally at `$XDG_DATA_HOME/cosmic-calendar/calendar.db` (directory mode `0700`). Zero network queries during panel popup rendering.
- **Virtual Instance Expansion:** Expands RRULE recurrences within queried date ranges, supporting `UNTIL`, `COUNT`, `EXDATE`, and detached recurring exceptions.
- **Meeting Link Detection:** Extracts Google Meet, Zoom, and Microsoft Teams links and opens them via `xdg-open` with Wayland activation token support.
- **Local `.ics` Support:** Reads standalone `.ics` files from `~/.local/share/calendars/` and system-wide holiday files from `/usr/share/calendar/`.
- **System Event Listeners:** Automatic resync on network connect and wake from suspend; instant cache invalidation on timezone updates.
- **Self-Healing Storage:** Automatically archives corrupted databases and initiates clean resynchronization without crash-looping.

---

## Building and Testing

### Prerequisites

- Rust 1.85+ (2024 Edition)
- `just` command runner
- System packages: `libsqlite3-dev`, `libdbus-1-dev`

### Compilation and Tests

```bash
# Run narrow test suites
cargo test --workspace

# Validate formatting and lints
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

The test suite includes an embedded `wiremock` CalDAV server testing full discovery, RFC 6578 sync, and multiget flows offline without live credentials.

---

## Installation

### Local User Install (Non-Root)

Installs the binaries, user systemd units, D-Bus activation services, and desktop entry to `~/.local`:

```bash
just install-user
```

Enable and start the user daemon:

```bash
systemctl --user daemon-reload
systemctl --user enable --now dev.cosmic.Calendar.service
```

### System-Wide Install (Root)

```bash
sudo just install
```

### Uninstallation

```bash
just uninstall-user
# or for system-wide:
sudo just uninstall
```

---

## D-Bus Interface (`dev.cosmic.Calendar`)

Service name: `dev.cosmic.Calendar`  
Object path: `/dev/cosmic/Calendar`  
Interface: `dev.cosmic.Calendar`

### Methods

- `GetCollections() -> Vec<CalendarCollection>`
- `SetCollectionVisibility(id: String, visible: bool) -> ()`
- `GetEvents(start: i64, end: i64) -> Vec<CalendarEventSummary>`
- `GetEventDetails(col_id: String, uid: String, rec_id: String) -> CalendarEventDetail`
- `Sync() -> ()`

### Signals

- `EventsChanged()`: Emitted when event data changes (debounced by 150ms).
- `CollectionsChanged()`: Emitted when collections are added, removed, or visibility changes.
- `SyncStatusChanged(is_syncing: bool)`: Emitted on sync cycle start and completion.

---

## License

- `calendar-client`: Mozilla Public License 2.0 ([MPL-2.0](https://www.mozilla.org/MPL/2.0/))
- `calendar-core`: Mozilla Public License 2.0 ([MPL-2.0](https://www.mozilla.org/MPL/2.0/))
- `cosmic-calendar-sync`: GNU General Public License v3.0 ([GPL-3.0-only](LICENSE))
- `cosmic-ext-applet-calendar`: GNU General Public License v3.0 ([GPL-3.0-only](LICENSE))
