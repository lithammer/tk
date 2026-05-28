//! `tk prime` — print the embedded agent-workflow briefing.
//!
//! No Repository Store precondition; safe for agent session-start hooks
//! before `tk init` has run. The briefing text lives at
//! `commands/prime.md` (a symlink to the source-of-truth markdown) and
//! is baked into the binary via `include_str!`.

use clap::Args as ClapArgs;

use crate::cli::Deps;

/// Embedded briefing. CR bytes are forbidden inside the file (LF only) and
/// trailing whitespace is trimmed so the rendered output ends with exactly
/// one `\n`.
const PRIME_RAW: &str = include_str!("prime.md");

#[derive(Debug, ClapArgs)]
pub struct Args {}

#[must_use]
pub fn run(deps: Deps<'_>, _args: Args) -> u8 {
    let trimmed = PRIME_RAW.trim_end_matches([' ', '\t', '\r', '\n']);
    let _ = deps.stdout.write_all(trimmed.as_bytes());
    let _ = deps.stdout.write_all(b"\n");
    0
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
    use crate::clock::FakeClock;
    use crate::proc::FakeRunner;
    use crate::render::Styler;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn prints_briefing_with_single_trailing_newline() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let code = run(
            Deps {
                stdout: &mut stdout,
                stderr: &mut stderr,
                stdin: &mut stdin,
                runner: &runner,
                clock: &clock,
                rng: &mut rng,
                cwd: cwd.as_path(),
                styler: Styler::plain(),
            },
            Args {},
        );
        assert_eq!(code, 0);
        let body = String::from_utf8(stdout).unwrap();
        assert!(body.ends_with('\n'));
        assert!(!body.ends_with("\n\n"));
        assert!(body.starts_with("# tk Workflow Context"));
        assert!(stderr.is_empty());
    }
}
