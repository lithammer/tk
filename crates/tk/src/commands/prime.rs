//! `tk prime` — print the embedded agent-workflow briefing when the current
//! directory has an initialized Repository Store.
//!
//! Gated on store presence so a global agent hook can run `tk prime` in any
//! directory: with no openable Repository Store it exits 0 with empty stdout
//! and stderr instead of printing (ADR-0020). The briefing text lives at
//! `commands/prime.md` and is baked into the binary via `include_str!`.

use clap::Args as ClapArgs;

use crate::cli::{Deps, Exit};
use crate::commands::resolver;

/// Embedded briefing. CR bytes are forbidden inside the file (LF only) and
/// trailing whitespace is trimmed so the rendered output ends with exactly
/// one `\n`.
const PRIME_RAW: &str = include_str!("prime.md");

#[derive(Debug, ClapArgs)]
pub struct Args {}

/// The briefing bytes: the embedded markdown trimmed to end with exactly one
/// `\n`. Pure and store-independent so the formatting contract is unit-tested
/// without opening a Repository Store.
fn briefing() -> String {
    let trimmed = PRIME_RAW.trim_end_matches([' ', '\t', '\r', '\n']);
    let mut out = String::with_capacity(trimmed.len() + 1);
    out.push_str(trimmed);
    out.push('\n');
    out
}

#[must_use]
pub fn run(deps: Deps<'_>, _args: Args) -> Exit {
    // Prime prints only when a Repository Store is initialized here; with no
    // openable store it exits 0 silently so a global agent hook stays quiet in
    // any directory (ADR-0020).
    if resolver::open_for_command(deps.runner, deps.cwd).is_ok() {
        let _ = deps.stdout.write_all(briefing().as_bytes());
    }
    Exit::Ok
}

#[cfg(test)]
mod tests {
    // The CR-byte guard runs at compile time; a CR in the briefing
    // would silently produce mixed line endings in agent output, so
    // catch it here rather than relying on a separate lint.
    const _: () = {
        let bytes = super::PRIME_RAW.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            assert!(bytes[i] != b'\r', "prime.md must not contain CR bytes");
            i += 1;
        }
    };

    use super::*;

    #[test]
    fn briefing_ends_with_single_trailing_newline() {
        let body = briefing();
        assert!(body.ends_with('\n'));
        assert!(!body.ends_with("\n\n"));
        assert!(body.starts_with("# tk Workflow Context"));
    }
}
