//! `tk manpage` — print or install the embedded `tk(1)` manpage.
//!
//! Default behavior writes the embedded bytes to stdout (caller can
//! pipe into `man -l -` or redirect to a file). `--install` copies the
//! same bytes to `<exe-dir>/../share/man/man1/tk.1` using an atomic
//! stage-and-rename that never deletes an existing target on failure;
//! `tk.1` is recreated by the rename step instead.

use std::io::Write;
use std::path::Path;

use clap::Args as ClapArgs;
use rand::Rng;

use crate::cli::Deps;
use crate::platform;

const MANPAGE_BYTES: &[u8] = include_bytes!("tk.1");

const STAGE_NAME_PREFIX: &str = ".tk.1.tmp.";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Install the manpage next to the running tk binary.
    #[arg(long)]
    pub install: bool,
}

#[must_use]
pub fn run(deps: Deps<'_>, args: Args) -> u8 {
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
    0
}

fn install(deps: Deps<'_>) -> u8 {
    let Deps {
        stdout,
        stderr,
        rng,
        ..
    } = deps;

    if platform::IS_WINDOWS {
        let _ = writeln!(stderr, "tk manpage: skipping install on Windows");
        return 0;
    }

    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "tk manpage: install failed: cannot resolve executable path: {err}"
            );
            return 1;
        }
    };
    let Some(exe_dir) = exe_path.parent() else {
        let _ = writeln!(
            stderr,
            "tk manpage: install failed: cannot resolve executable path: executable path has no parent directory"
        );
        return 1;
    };

    let target_dir = exe_dir.join("..").join("share").join("man").join("man1");
    let target_path = target_dir.join("tk.1");

    if let Err(err) = std::fs::create_dir_all(&target_dir) {
        render_install_failure(stderr, &target_path, &err.to_string());
        return 1;
    }

    let stage_name = render_stage_name(rng);
    let stage_path = target_dir.join(&stage_name);

    if let Err(err) = std::fs::write(&stage_path, MANPAGE_BYTES) {
        let _ = std::fs::remove_file(&stage_path);
        render_install_failure(stderr, &target_path, &err.to_string());
        return 1;
    }

    if let Err(err) = std::fs::rename(&stage_path, &target_path) {
        let _ = std::fs::remove_file(&stage_path);
        render_install_failure(stderr, &target_path, &err.to_string());
        return 1;
    }

    let _ = writeln!(stdout, "Installed manpage at {}", target_path.display());
    0
}

fn render_install_failure<W: Write + ?Sized>(stderr: &mut W, path: &Path, reason: &str) {
    let _ = writeln!(
        stderr,
        "tk manpage: install failed at {}: {reason}; existing file (if any) left unchanged",
        path.display()
    );
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
            Args { install: false },
        );
        assert_eq!(code, 0);
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
