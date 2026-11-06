//! The clock `now()` and `nowUtc()` read (RFC-0049).
//!
//! The interpreter owns one, shared by every lowered call. By default it is
//! the system's. A host (a test, `byard shot --now`) can **fix** it at a wall
//! time, and from then on it reads that time plus the app time that has gone
//! by since: the same app time `set_now_ms` already moves for animations and
//! that `byard shot --at` advances. There is no second clock to keep in step.

use std::cell::Cell;

use chrono::{Duration, NaiveDate, NaiveDateTime};

use super::dates;

/// The wall clock of an interpreter.
#[derive(Debug, Default)]
pub struct Clock {
    /// `Some(time)` once fixed: the wall time at `anchor_ms` of app time.
    fixed: Cell<Option<NaiveDateTime>>,
    /// App time, in ms, as the host last set it.
    app_ms: Cell<u32>,
    /// The app time at which the clock was fixed.
    anchor_ms: Cell<u32>,
}

impl Clock {
    /// Records the app time, which is what moves a fixed clock.
    pub fn set_app_ms(&self, ms: u32) {
        self.app_ms.set(ms);
    }

    /// Fixes the clock at `text` (an ISO date, or date-time with or without
    /// seconds), as of the app time now.
    ///
    /// # Errors
    ///
    /// Returns a message when `text` is not an ISO date or date-time.
    pub fn fix(&self, text: &str) -> Result<(), String> {
        let when = dates::parse_fields(text).ok_or_else(|| {
            format!("`{text}` is not a date and time; write it as 2026-11-05T09:00:00")
        })?;
        let time =
            NaiveDate::from_ymd_opt(i32::try_from(when.0).unwrap_or(i32::MAX), when.1, when.2)
                .and_then(|d| d.and_hms_opt(when.3, when.4, when.5))
                .ok_or_else(|| format!("`{text}` is not a date and time"))?;
        self.fixed.set(Some(time));
        self.anchor_ms.set(self.app_ms.get());
        Ok(())
    }

    /// The local wall clock, `YYYY-MM-DDTHH:MM:SS`.
    #[must_use]
    pub fn local(&self) -> String {
        let time = self
            .fixed_now()
            .unwrap_or_else(|| chrono::Local::now().naive_local());
        time.format("%Y-%m-%dT%H:%M:%S").to_string()
    }

    /// UTC, `YYYY-MM-DDTHH:MM:SSZ`. A fixed clock has no zone, so it is the
    /// fixed time with the `Z`.
    #[must_use]
    pub fn utc(&self) -> String {
        let time = self
            .fixed_now()
            .unwrap_or_else(|| chrono::Utc::now().naive_utc());
        time.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    /// The fixed time advanced by the app time since it was fixed.
    fn fixed_now(&self) -> Option<NaiveDateTime> {
        let base = self.fixed.get()?;
        let elapsed = self.app_ms.get().saturating_sub(self.anchor_ms.get());
        Some(base + Duration::milliseconds(i64::from(elapsed)))
    }
}
