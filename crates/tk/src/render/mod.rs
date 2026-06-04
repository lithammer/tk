//! Terminal-rendering subsystem: palette, policy-aware emitter, and
//! sanitisers for Repository Store text.
//!
//! ADR-0014 records the contracts this module preserves: named semantic
//! styles drawn from a single palette, a single colour policy resolved
//! once and carried on `Deps`, and plain output when the active stream
//! is not a colour-capable terminal.
//!
//! Layout:
//!
//! - [`palette`]: named [`anstyle::Style`] constants the rest of `tk`
//!   reaches for. One file owns every colour decision so retheming is a
//!   single diff. Per ADR-0014, the anti-drift rule rejects
//!   per-call-site chaining (`owo-colors`-style); call sites take a
//!   palette name and let the resolved [`styler::Styler`] gate emission.
//! - [`styler`]: the policy-aware emitter. Holds per-stream
//!   [`styler::ColorChoice`] values resolved once from `--color` /
//!   `NO_COLOR` / `CLICOLOR_FORCE` / TTY detection and carried on `Deps`.
//! - [`sanitize`]: write user/Remote-controlled text with terminal
//!   control bytes rendered inert.

pub mod highlight;
pub mod palette;
pub mod sanitize;
pub mod styler;

pub use styler::{ColorChoice, Styler, resolve_choice, resolve_styler_from_env};
