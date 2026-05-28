//! Injectable wall clock returning UTC milliseconds since the Unix epoch.
//!
//! ADR-0018 names a process-level determinism seam (`TK_NOW` /
//! `SOURCE_DATE_EPOCH`) so the Rust binary can be driven from txtar scenarios
//! without an in-process clock fake. The trait keeps the in-process seam too
//! so command-handler tests can substitute a `FakeClock`.
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
    /// The 24-byte fixed width matches the Zig oracle's `nowIso`.
    fn now_iso(&self) -> String {
        format_iso(self.now_ms())
    }
}

/// Production clock. Reads `TK_NOW` at construction time so a deterministic
/// override survives for the lifetime of the process — the env var is parsed
/// once, not on every `now_ms` call.
///
/// Accepted forms:
/// - `TK_NOW=<iso8601>`: any RFC 9557 / ISO-8601 timestamp jiff can parse,
///   including offset-shifted forms (`+02:00`) which are normalized to UTC
///   milliseconds. The output value (rendered back via `now_iso`) is always
///   the UTC equivalent — pin `Z`-suffixed inputs in scenario fixtures if
///   that matters for byte-exact comparison.
/// - `TK_NOW=<unix_ms>`: integer milliseconds since the epoch.
/// - `SOURCE_DATE_EPOCH=<unix_seconds>`: integer seconds since the epoch
///   (the reproducible-builds convention).
///
/// `TK_NOW` takes precedence over `SOURCE_DATE_EPOCH` when both are set. An
/// unparseable env var falls through silently to the wall clock — callers
/// that need fail-fast semantics should validate before launch.
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
        self.pinned_ms.unwrap_or_else(|| Timestamp::now().as_millisecond())
    }
}

fn read_clock_override() -> Option<i64> {
    if let Ok(v) = std::env::var("TK_NOW") {
        if let Some(ms) = parse_tk_now(&v) {
            return Some(ms);
        }
    }
    if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH") {
        // `checked_mul` keeps the rendered output within jiff's representable
        // range. Overflow (or any other parse failure) falls through to the
        // wall clock — the env var was almost certainly a unit confusion
        // (user wrote ms but the convention is seconds), so wall-clock
        // semantics are the safer default than e.g. clamping to i64::MAX ms.
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
    trimmed.parse::<Timestamp>().ok().map(Timestamp::as_millisecond)
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
        assert_eq!(format_iso(1_778_284_800_000), "2026-05-09T00:00:00.000Z");
    }

    #[test]
    fn parse_tk_now_accepts_integer_millis() {
        assert_eq!(parse_tk_now("0"), Some(0));
        assert_eq!(parse_tk_now("1778457600000"), Some(1_778_457_600_000));
    }

    #[test]
    fn parse_tk_now_accepts_canonical_iso_form() {
        // Round-trip the 24-byte form tk emits.
        let canonical = "2026-05-09T12:34:56.789Z";
        let ms = parse_tk_now(canonical).expect("canonical form must parse");
        assert_eq!(format_iso(ms), canonical);
    }

    #[test]
    fn fake_clock_returns_pinned_value() {
        let c = FakeClock::new(42);
        assert_eq!(c.now_ms(), 42);
    }
}
