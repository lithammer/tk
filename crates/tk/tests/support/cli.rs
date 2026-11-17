//! Test-only process entry point for real-Git command scenarios.
use std::path::Path;
use std::process::ExitCode;

use rand::SeedableRng;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = std::path::PathBuf::from(std::env::var_os("TK_TEST_ROOT").unwrap());
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut stdin = std::io::empty();
    let runner = InjectedRunner(tk::proc::RealRunner::new());
    let clock = tk::clock::RealClock::new();
    let mut rng = rand::rngs::StdRng::try_from_rng(&mut rand::rngs::SysRng).unwrap();

    if let Ok(seed) = std::env::var("TK_TEST_SEED") {
        rng = rand::rngs::StdRng::seed_from_u64(seed.parse().unwrap());
    }

    let cwd = std::env::current_dir().unwrap();
    let data_root = match std::env::var("TK_TEST_DATA_ROOT") {
        Ok(value) if value == "missing" => None,
        Ok(value) if value == "native" => dirs::data_local_dir(),
        Ok(value) => Some(std::path::PathBuf::from(value)),
        Err(_) => Some(root.join("data")),
    };
    let deps = tk::cli::Deps {
        stdout: &mut stdout,
        stderr: &mut stderr,
        stdin: &mut stdin,
        runner: &runner,
        clock: &clock,
        rng: &mut rng,
        cwd: &cwd,
        data_root: data_root.as_deref(),
        migration_boundary,
        styler: tk::render::resolve_styler_from_env(),
    };

    let exit = tk::cli::run_argv(deps, &args).unwrap();

    ExitCode::from(exit.code())
}

struct InjectedRunner(tk::proc::RealRunner);

impl tk::proc::ProcRunner for InjectedRunner {
    fn run(&self, argv: &[&str], cwd: &Path) -> Result<tk::proc::RunOutput, tk::proc::ProcError> {
        let failure = std::env::var("TK_TEST_GIT_FAILURE").unwrap_or_default();
        if (failure == "former" && argv.contains(&"--git-dir"))
            || (failure == "replace-before" && argv.contains(&"--replace-all"))
        {
            return Err(tk::proc::ProcError::SpawnFailed);
        }
        let out = self.0.run(argv, cwd)?;
        if failure == "replace-after" && argv.contains(&"--replace-all") {
            return Err(tk::proc::ProcError::OutcomeUnobserved);
        }
        Ok(out)
    }

    fn run_with_stdin(
        &self,
        argv: &[&str],
        cwd: &Path,
        stdin: &[u8],
    ) -> Result<tk::proc::RunOutput, tk::proc::ProcError> {
        self.0.run_with_stdin(argv, cwd, stdin)
    }
}

fn migration_boundary(step: tk::store::relocation::Boundary) -> std::io::Result<()> {
    let failure = std::env::var("TK_TEST_MIGRATION_FAILURE").unwrap_or_default();
    if failure == format!("crash:{step:?}") {
        eprint!("interrupted at {step:?}");
        std::process::exit(99);
    }
    if failure == format!("pause:{step:?}") {
        let gate = std::path::PathBuf::from(std::env::var_os("TK_TEST_MIGRATION_GATE").unwrap());
        std::fs::write(gate.join("ready"), "").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !gate.join("release").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "migration test must release its gate"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    for (prefix, kind) in [
        ("full", std::io::ErrorKind::StorageFull),
        ("deny", std::io::ErrorKind::PermissionDenied),
    ] {
        if failure == format!("{prefix}:{step:?}") {
            return Err(kind.into());
        }
    }
    if failure == format!("{step:?}") {
        return Err(std::io::Error::other(format!("interrupted at {step:?}")));
    }
    Ok(())
}
