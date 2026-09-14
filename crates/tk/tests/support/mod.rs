//! Run the command seam in a child process with real Git and an isolated data root.
use std::path::Path;
use std::process::{Command, Output};

/// Dispatch with real Git in an isolated child; return CLI streams and exit status.
pub fn run(cwd: &Path, root: &Path, args: &[String], env: &[(&str, &str)]) -> Output {
    let capture = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "support::cli_child", "--ignored"])
        .current_dir(cwd)
        .env("TK_TEST_ARGS", serde_json::to_string(args).unwrap())
        .env("TK_TEST_ROOT", root)
        .env("TK_TEST_CAPTURE", capture.path())
        .env("GIT_CONFIG_GLOBAL", root.join("global.gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("TK_TEST_DATA_ROOT")
        .env_remove("TK_TEST_SEED")
        .env_remove("TK_TEST_GIT_FAILURE")
        .env_remove("TK_TEST_MIGRATION_FAILURE")
        .env_remove("TK_TEST_MIGRATION_GATE")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("TK_SCOPE")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    Output {
        status: output.status,
        stdout: std::fs::read(capture.path().join("stdout")).expect("child must finish dispatch"),
        stderr: std::fs::read(capture.path().join("stderr")).unwrap(),
    }
}

#[test]
#[ignore = "subprocess entry point for the real-Git command seam"]
fn cli_child() {
    use rand::SeedableRng;
    let Ok(args) = std::env::var("TK_TEST_ARGS") else {
        return;
    };
    let args: Vec<String> = serde_json::from_str(&args).unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("TK_TEST_ROOT").unwrap());
    let capture = std::path::PathBuf::from(std::env::var_os("TK_TEST_CAPTURE").unwrap());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
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
    std::fs::write(capture.join("stdout"), stdout).unwrap();
    std::fs::write(capture.join("stderr"), stderr).unwrap();
    std::process::exit(i32::from(exit.code()));
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
        let capture = std::path::PathBuf::from(std::env::var_os("TK_TEST_CAPTURE").unwrap());
        std::fs::write(capture.join("stdout"), "").unwrap();
        std::fs::write(capture.join("stderr"), format!("interrupted at {step:?}")).unwrap();
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
