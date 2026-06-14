//! `tk prime` — print the embedded agent-workflow briefing when the current
//! directory has an initialized Repository Store.
//!
//! Gated on store presence so a global agent hook can run `tk prime` in any
//! directory: with no openable Repository Store it exits 0 with empty stdout
//! and stderr instead of printing (ADR-0020). The briefing text lives at
//! `commands/prime.md` and is baked into the binary via `include_str!`.

use clap::Args as ClapArgs;

use crate::cli::{CommandError, Deps, Exit};
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

/// Run `tk prime`. Never fails: with no openable Repository Store it exits 0
/// silently so a global agent hook stays quiet in any directory (ADR-0020), so
/// it never returns a [`CommandError`] — the signature matches the seam only
/// for dispatch uniformity.
pub fn run(deps: &mut Deps<'_>, _args: Args) -> Result<Exit, CommandError> {
    // Prime prints only when a Repository Store is initialized here.
    if resolver::open_for_command(deps.runner, deps.cwd, deps.clock).is_ok() {
        let _ = deps.stdout.write_all(briefing().as_bytes());
    }
    Ok(Exit::Ok)
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
    use crate::proc::{FakeRunner, RunOutput};
    use crate::render::Styler;
    use crate::store::migrations;
    use crate::store::testing::TmpStore;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rusqlite::Connection;

    #[test]
    fn briefing_ends_with_single_trailing_newline() {
        let body = briefing();
        assert!(body.ends_with('\n'));
        assert!(!body.ends_with("\n\n"));
        assert!(body.starts_with("# tk Workflow Context"));
    }

    #[test]
    fn prime_against_a_behind_version_store_migrates_and_prints_the_briefing() {
        // Accepted side effect (tk-110, tracked further by tk-112): with
        // auto-migrate-on-open, a passive `tk prime` against a store written by
        // an older binary upgrades the schema and still prints the briefing —
        // consistent with "tk owns its format". ADR-0020 keeps prime silent
        // only when no store can be opened.
        let store = TmpStore::new("repo");
        std::fs::create_dir_all(store.tk_dir()).unwrap();
        let mut conn = Connection::open(store.db_path()).unwrap();
        conn.execute_batch("pragma foreign_keys = on").unwrap();
        migrations::apply_through(&mut conn, 2, "2026-05-09T00:00:00.000Z").unwrap();
        drop(conn);

        let cwd_path = std::env::current_dir().unwrap();
        let runner = FakeRunner::new();
        runner.expect(
            &["git", "rev-parse"],
            RunOutput {
                exit_code: 0,
                stdout: store.git_rev_parse_stdout(),
                stderr: Vec::new(),
            },
        );
        let clock = FakeClock::new(1_778_284_800_000);
        let mut rng = StdRng::seed_from_u64(0);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut deps = Deps {
            stdout: &mut stdout,
            stderr: &mut stderr,
            stdin: &mut stdin,
            runner: &runner,
            clock: &clock,
            rng: &mut rng,
            cwd: &cwd_path,
            styler: Styler::plain(),
        };

        assert_eq!(run(&mut deps, Args {}).unwrap(), Exit::Ok);
        assert!(
            String::from_utf8(stdout)
                .unwrap()
                .starts_with("# tk Workflow Context")
        );

        let conn = Connection::open(store.db_path()).unwrap();
        assert_eq!(
            migrations::current_version(&conn).unwrap(),
            i64::from(migrations::MAX_KNOWN_VERSION)
        );
    }
}
