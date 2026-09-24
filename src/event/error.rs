// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

#[derive(Debug, thiserror::Error, Clone)]
pub enum CalendarError {
    #[error("I/O error: {0}")]
    Io(String),

    #[error("iCalendar parse error: {0}")]
    Parse(String),

    #[error("D-Bus error: {0}")]
    Dbus(String),

    #[error("Calendar source not found: {0}")]
    NotFound(String),
}

impl From<std::io::Error> for CalendarError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<zbus::Error> for CalendarError {
    fn from(err: zbus::Error) -> Self {
        Self::Dbus(err.to_string())
    }
}
