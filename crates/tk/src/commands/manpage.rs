//! `tk manpage` — print or install the embedded `tk(1)` manpage.
//!
//! Default behavior writes the embedded bytes to stdout (caller can
//! pipe into `man -l -` or redirect to a file). `--install` copies the
//! same bytes to `<prefix>/share/man/man1/tk.1` — where `<prefix>` is
//! the parent of the executable's directory (e.g. `/usr/local` for a
//! `/usr/local/bin/tk` install) — using an atomic stage-and-rename that
//! never deletes an existing target on failure; `tk.1` is recreated by
//! the rename step instead.

use std::path::Path;

use clap::Args as ClapArgs;
use rand::Rng;

use crate::cli::{CommandError, Deps, Exit};
use crate::platform;

const MANPAGE_BYTES: &[u8] = include_bytes!("tk.1");

const STAGE_NAME_PREFIX: &str = ".tk.1.tmp.";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Install the manpage next to the running tk binary.
    #[arg(long)]
    pub install: bool,
}

pub fn run(deps: &mut Deps<'_>, args: Args) -> Result<Exit, CommandError> {
    // CR-byte guard at startup so a future change to tk.1 can't slip in
    // CRLF endings unnoticed.
    debug_assert!(
        !MANPAGE_BYTES.contains(&b'\r'),
        "tk.1 must use LF line endings (no CR bytes)"
    );

    if args.install {
        return install(deps);
    }
    let _ = deps.stdout.write_all(MANPAGE_BYTES);
    Ok(Exit::Ok)
}

fn install(deps: &mut Deps<'_>) -> Result<Exit, CommandError> {
    if platform::IS_WINDOWS {
        // Informational success, not a diagnostic: written straight to stderr
        // on the Ok path, so it keeps its own `tk manpage:` prefix (the seam
        // frames only Err).
        let _ = writeln!(deps.stderr, "tk manpage: skipping install on Windows");
        return Ok(Exit::Ok);
    }

    let exe_path = std::env::current_exe().map_err(|err| {
        CommandError::failure(format!(
            "install failed: cannot resolve executable path: {err}"
        ))
    })?;
    let Some(exe_dir) = exe_path.parent() else {
        return Err(CommandError::failure(
            "install failed: cannot resolve executable path: executable path has no parent directory",
        ));
    };

    // Install prefix is the parent of the executable's directory
    // (`/usr/local/bin/tk` -> `/usr/local`). Computing the parent
    // directly, rather than appending a `..` segment, keeps the
    // displayed path clean (`/usr/local/share/...`). Fall back to the
    // executable's directory if it has no parent (e.g. a root install).
    let install_prefix = exe_dir.parent().unwrap_or(exe_dir);
    let target_dir = install_prefix.join("share").join("man").join("man1");
    let target_path = target_dir.join("tk.1");

    if let Err(err) = std::fs::create_dir_all(&target_dir) {
        return Err(install_failure(&target_path, &err.to_string()));
    }

    let stage_name = render_stage_name(deps.rng);
    let stage_path = target_dir.join(&stage_name);

    if let Err(err) = std::fs::write(&stage_path, MANPAGE_BYTES) {
        let _ = std::fs::remove_file(&stage_path);
        return Err(install_failure(&target_path, &err.to_string()));
    }

    if let Err(err) = std::fs::rename(&stage_path, &target_path) {
        let _ = std::fs::remove_file(&stage_path);
        return Err(install_failure(&target_path, &err.to_string()));
    }

    let _ = writeln!(
        deps.stdout,
        "Installed manpage at {}",
        target_path.display()
    );
    Ok(Exit::Ok)
}

fn install_failure(path: &Path, reason: &str) -> CommandError {
    CommandError::failure(format!(
        "install failed at {}: {reason}; existing file (if any) left unchanged",
        path.display()
    ))
}

/// Build the hex-suffixed staged filename. 64 random bits make concurrent
/// installs collision-free without pid sniffing.
fn render_stage_name<R: Rng + ?Sized>(rng: &mut R) -> String {
    let mut bytes = [0u8; 8];
    rng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(STAGE_NAME_PREFIX.len() + 16);
    s.push_str(STAGE_NAME_PREFIX);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::proc::FakeRunner;
    use crate::render::Styler;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn prints_embedded_manpage_bytes_to_stdout() {
        let cwd = std::env::current_dir().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let runner = FakeRunner::new();
        let clock = FakeClock::new(0);
        let mut rng = StdRng::seed_from_u64(0);
        let mut deps = Deps {
            stdout: &mut stdout,
            stderr: &mut stderr,
            stdin: &mut stdin,
            runner: &runner,
            clock: &clock,
            rng: &mut rng,
            cwd: cwd.as_path(),
            styler: Styler::plain(),
        };
        let code = run(&mut deps, Args { install: false }).unwrap();
        assert_eq!(code, Exit::Ok);
        assert_eq!(stdout.as_slice(), MANPAGE_BYTES);
        assert!(stderr.is_empty());
    }

    #[test]
    fn stage_name_has_prefix_and_hex_suffix() {
        let mut rng = StdRng::seed_from_u64(0);
        let name = render_stage_name(&mut rng);
        assert!(name.starts_with(STAGE_NAME_PREFIX));
        let suffix = &name[STAGE_NAME_PREFIX.len()..];
        assert_eq!(suffix.len(), 16);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
