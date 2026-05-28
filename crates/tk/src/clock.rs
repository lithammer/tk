//! Injectable wall clock returning UTC milliseconds since the Unix epoch.
//!
//! ADR-0018 names a process-level determinism seam (`TK_NOW` /
//! `SOURCE_DATE_EPOCH`) so the Rust binary can be driven from txtar scenarios
//! without an in-process clock fake. The trait shape mirrors `clock.zig` so
//! command-handler tests can still substitute a `FakeClock`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Common clock seam. `tk init` reads this to stamp `schema_migrations.applied_at`.
pub trait Clock {
    /// UTC milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;

    /// ISO-8601 UTC millisecond-precision rendering: `YYYY-MM-DDTHH:MM:SS.fffZ`.
    /// The 24-byte fixed width matches the Zig oracle's `nowIso`.
    fn now_iso(&self) -> String {
        format_iso(self.now_ms())
    }
}

/// Production clock. Reads `TK_NOW` at construction time so a deterministic
/// override survives for the lifetime of the process — the env var is parsed
/// once, not on every `now_ms` call.
///
/// Accepted forms (matching ADR-0018's determinism seams):
/// - `TK_NOW=<iso8601>`: `2026-05-09T00:00:00.000Z` (millisecond precision).
/// - `TK_NOW=<unix_ms>`: integer milliseconds since the epoch.
/// - `SOURCE_DATE_EPOCH=<unix_seconds>`: integer seconds since the epoch
///   (the reproducible-builds convention; promoted to ms internally).
///
/// `TK_NOW` takes precedence over `SOURCE_DATE_EPOCH` when both are set.
pub struct RealClock {
    pinned_ms: Option<i64>,
}

impl RealClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pinned_ms: read_clock_override(),
        }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn now_ms(&self) -> i64 {
        if let Some(ms) = self.pinned_ms {
            return ms;
        }
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        i64::try_from(dur.as_millis()).unwrap_or(i64::MAX)
    }
}

fn read_clock_override() -> Option<i64> {
    if let Ok(v) = std::env::var("TK_NOW") {
        if let Some(ms) = parse_tk_now(&v) {
            return Some(ms);
        }
    }
    if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH") {
        // Reject overflow rather than silently clamping to i64::MAX ms (which
        // would render as a year in the 292,000,000s range). A SOURCE_DATE_EPOCH
        // that overflows when promoted to ms is almost certainly a typo
        // (user wrote ms but the env var convention is seconds).
        if let Ok(secs) = v.trim().parse::<i64>() {
            if let Some(ms) = secs.checked_mul(1000) {
                return Some(ms);
            }
        }
    }
    None
}

fn parse_tk_now(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if let Ok(ms) = trimmed.parse::<i64>() {
        return Some(ms);
    }
    parse_iso_millis(trimmed)
}

/// Parse the exact 24-byte ISO form `tk` emits: `YYYY-MM-DDTHH:MM:SS.fffZ`.
///
/// Strict on purpose: the determinism seam should reject anything that isn't
/// the canonical shape. Note that an unparseable `TK_NOW` value bubbles up as
/// `None` and [`read_clock_override`] then falls back to the wall clock — the
/// strictness is in *what* is accepted, not in panicking on rejection.
/// Production callers (scenario harnesses) should validate the env var before
/// launch if silent fallback is unacceptable.
fn parse_iso_millis(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 24 {
        return None;
    }
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'.' || bytes[23] != b'Z' {
        return None;
    }
    let year: i32 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let minute: u32 = s[14..16].parse().ok()?;
    let second: u32 = s[17..19].parse().ok()?;
    let millis: u32 = s[20..23].parse().ok()?;
    Some(civil_to_unix_ms(year, month, day, hour, minute, second, millis))
}

/// Render `unix_ms` as `YYYY-MM-DDTHH:MM:SS.fffZ`.
///
/// Implemented locally to avoid a chrono/time dependency in slice 0; the
/// rendering surface is single-purpose (schema_migrations.applied_at).
#[must_use]
pub fn format_iso(unix_ms: i64) -> String {
    let total_secs = unix_ms.div_euclid(1000);
    // `rem_euclid(1000)` always returns a non-negative i64 in `[0, 999]`, so
    // the u32 cast can never truncate or lose sign.
    let millis = u32::try_from(unix_ms.rem_euclid(1000)).unwrap_or(0);
    let (year, month, day, hour, minute, second) = unix_to_civil(total_secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's `days_from_civil`, returning Unix milliseconds.
///
/// The argument types are intentionally narrow (`u32` for sub-day components,
/// `i32` for year). All casts inside the body are between values already
/// bounded by the calendar arithmetic, so the truncation/sign-loss clippy
/// pedantic lints are knowingly suppressed.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn civil_to_unix_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32, millis: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m = month;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = i64::from(era) * 146_097 + i64::from(doe) - 719_468;
    let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    secs * 1000 + i64::from(millis)
}

/// Inverse of `civil_to_unix_ms` (seconds in, civil components out).
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn unix_to_civil(unix_secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = unix_secs.div_euclid(86_400) + 719_468;
    let sod = unix_secs.rem_euclid(86_400);
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = (days - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i32) + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let hour = (sod / 3600) as u32;
    let minute = ((sod % 3600) / 60) as u32;
    let second = (sod % 60) as u32;
    (year, month, day, hour, minute, second)
}

// ---- Fake ---------------------------------------------------------------

/// In-process fake for command-handler tests. Returns `pinned_ms` from every
/// `now_ms`. The matching env-var seam (`TK_NOW`) is exercised through
/// `RealClock`; tests that exercise that branch construct a process via
/// `assert_cmd` rather than calling the fake directly.
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
    fn format_iso_matches_zig_oracle_shape() {
        // 2026-05-09T00:00:00.000Z corresponds to 1_778_284_800_000 ms.
        // Mirrors the canonical `applied_at` stamp used in the Zig migration
        // tests so byte-stability is preserved.
        assert_eq!(format_iso(1_778_284_800_000), "2026-05-09T00:00:00.000Z");
    }

    #[test]
    fn parse_iso_millis_round_trip() {
        let canonical = "2026-05-09T12:34:56.789Z";
        let ms = parse_iso_millis(canonical).expect("canonical form must parse");
        assert_eq!(format_iso(ms), canonical);
    }

    #[test]
    fn parse_iso_millis_rejects_non_canonical_forms() {
        // The determinism seam should be strict — tk only emits the 24-byte
        // form, so a fixture with extra precision is a typo, not a feature.
        assert!(parse_iso_millis("2026-05-09T00:00:00Z").is_none());
        assert!(parse_iso_millis("2026-05-09 00:00:00.000Z").is_none());
        assert!(parse_iso_millis("2026-05-09T00:00:00.000+00:00").is_none());
    }

    #[test]
    fn parse_tk_now_accepts_integer_millis() {
        assert_eq!(parse_tk_now("0"), Some(0));
        assert_eq!(parse_tk_now("1778457600000"), Some(1_778_457_600_000));
    }

    #[test]
    fn fake_clock_returns_pinned_value() {
        let c = FakeClock::new(42);
        assert_eq!(c.now_ms(), 42);
    }
}
