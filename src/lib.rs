// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

pub mod config;

pub mod event;
mod localize;
mod time;
mod window;

pub use window::{AppletModeTrait, StandaloneCalendar, TimeReplacement, Window};

pub fn run() -> cosmic::iced::Result {
    run_standalone()
}

pub fn run_standalone() -> cosmic::iced::Result {
    localize::localize();

    cosmic::applet::run::<Window<StandaloneCalendar>>(())
}

pub fn run_time_replacement() -> cosmic::iced::Result {
    localize::localize();

    cosmic::applet::run::<Window<TimeReplacement>>(())
}
