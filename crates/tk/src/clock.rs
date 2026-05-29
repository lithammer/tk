//! Injectable wall clock returning UTC milliseconds since the Unix epoch.
//!
//! The [`Clock`] trait is the determinism seam: production uses [`RealClock`]
//! (the system wall clock); command-handler tests substitute a [`FakeClock`].
//! Subprocess scenario tests run the real clock and redact any timestamp they
//! surface — the binary exposes no clock override env var (ADR-0018, amended
//! by tk-105).
//!
//! Formatting and parsing are delegated to `jiff` (`Timestamp::parse`,
//! `from_millisecond`, `strftime`) — calendar arithmetic edge cases on
//! pre-1970 dates and far-future stamps that the original hand-roll mishandled
//! are jiff's problem now. tk doesn't use jiff's timezone or zoned-datetime
//! machinery, so the dep is pinned with `default-features = false` to keep
//! the tzdb bundle out of the static binary.

use jiff::Timestamp;

/// Common clock seam. `tk init` reads this to stamp `schema_migrations.applied_at`.
pub trait Clock {
    /// UTC milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;

    /// ISO-8601 UTC millisecond-precision rendering: `YYYY-MM-DDTHH:MM:SS.fffZ`.
    /// The 24-byte fixed width is the contract every stored timestamp honours.
    fn now_iso(&self) -> String {
        format_iso(self.now_ms())
    }
}

/// Production clock backed by the system wall clock.
#[derive(Default)]
pub struct RealClock;

impl RealClock {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Clock for RealClock {
    fn now_ms(&self) -> i64 {
        Timestamp::now().as_millisecond()
    }
}

/// Render `unix_ms` as `YYYY-MM-DDTHH:MM:SS.fffZ`.
///
/// Slice-0 scenarios pin this exact 24-byte shape via `schema_migrations.applied_at`.
/// Jiff's `Timestamp` Display prints either second or nanosecond precision
/// depending on the fractional part, so we use an explicit strftime to
/// guarantee the fixed 24-byte width.
///
/// Panics if `unix_ms` is outside jiff's representable `Timestamp` range
/// (roughly years -9999..=9999). That bound covers every value tk can
/// legitimately produce — `Clock::now_ms` reads from `SystemTime` or a fake,
/// both of which stay in-range — so a panic here is a real bug, not a user-
/// input error.
#[must_use]
pub fn format_iso(unix_ms: i64) -> String {
    let ts = Timestamp::from_millisecond(unix_ms)
        .expect("unix_ms must be within jiff's Timestamp range");
    ts.strftime("%Y-%m-%dT%H:%M:%S.%3fZ").to_string()
}

// ---- Fake ---------------------------------------------------------------

/// In-process fake for command-handler tests. Returns `pinned_ms` from every
/// `now_ms`. Subprocess scenario tests use [`RealClock`] and redact any
/// timestamp they surface, so there is no env-var clock branch to fake.
pub struct FakeClock {
    pub pinned_ms: i64,
}

impl FakeClock {
    #[must_use]
    pub fn new(pinned_ms: i64) -> Self {
        Self { pinned_ms }
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.pinned_ms
    }
}

// ---- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_iso_renders_iso8601_milliseconds() {
        // 2026-05-09T00:00:00.000Z corresponds to 1_778_284_800_000 ms.
        assert_eq!(format_iso(1_778_284_800_000), "2026-05-09T00:00:00.000Z");
    }

    #[test]
    fn fake_clock_returns_pinned_value() {
        let c = FakeClock::new(42);
        assert_eq!(c.now_ms(), 42);
    }
}
