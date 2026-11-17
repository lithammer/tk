//! Run the command seam in a child process with real Git and an isolated data root.
use std::path::Path;
use std::process::{Command, Output};

/// Dispatch with real Git in an isolated child; return CLI streams and exit status.
pub fn run(cwd: &Path, root: &Path, args: &[String], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tk-test-helper"));
    command
        .args(args)
        .current_dir(cwd)
        .env("TK_TEST_ROOT", root)
        .env("GIT_CONFIG_GLOBAL", global_config(root))
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

    command.output().unwrap()
}

/// Git rejects Windows verbatim paths in GIT_CONFIG_GLOBAL.
pub fn global_config(root: &Path) -> String {
    let path = root.join("global.gitconfig");
    let text = path.to_str().expect("UTF-8 test path");
    if cfg!(windows) {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(drive) = text.strip_prefix(r"\\?\") {
            return drive.to_string();
        }
    }
    text.to_string()
}
