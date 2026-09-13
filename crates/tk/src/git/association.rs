//! Repository-local Store pointers and credential-free Association Evidence.
use std::path::Path;

use crate::proc::ProcRunner;

/// Git config failures omit stderr because config diagnostics can contain credentials.
#[derive(Debug, thiserror::Error)]
#[error("failed to {0} repository-local Store config")]
pub struct ConfigError(&'static str);

/// Read every local pointer, excluding includes, global config, and worktree config.
pub fn pointers<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
) -> Result<Vec<String>, ConfigError> {
    values(runner, cwd, &["--get-all", "tk.storeId"])
}

/// Add a pointer only after publication. Never replace another pointer's value.
pub fn install<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    id: &str,
) -> Result<(), ConfigError> {
    if !pointers(runner, cwd)?.is_empty() {
        return Err(ConfigError("install"));
    }
    let output = runner
        .run(
            &[
                "git",
                "config",
                "--local",
                "--no-includes",
                "--add",
                "tk.storeId",
                id,
            ],
            cwd,
        )
        .map_err(|_| ConfigError("install"))?;
    if !output.succeeded() || pointers(runner, cwd)? != [id] {
        return Err(ConfigError("install"));
    }
    Ok(())
}

/// Retain exact public transport URLs only, sorted and deduplicated.
/// Userinfo, queries, fragments, local paths, and helper syntax are omitted.
pub fn remote_urls<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
) -> Result<Vec<String>, ConfigError> {
    let mut urls: Vec<_> = values(runner, cwd, &["--get-regexp", r"^remote\..*\.url$"])?
        .into_iter()
        .filter_map(|entry| entry.split_once('\n').map(|(_, url)| url.to_string()))
        .filter(|url| {
            let Some((scheme, rest)) = url.split_once("://") else {
                return false;
            };
            matches!(scheme, "https" | "http" | "git" | "ssh")
                && !rest.is_empty()
                && !url.contains(['@', '?', '#'])
                && !url.chars().any(char::is_control)
        })
        .collect();
    urls.sort();
    urls.dedup();
    Ok(urls)
}

/// Explicit recovery replaces all broken local values under the lifecycle lock.
pub fn replace<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    id: &str,
) -> Result<(), ConfigError> {
    let out = runner
        .run(
            &[
                "git",
                "config",
                "--local",
                "--no-includes",
                "--replace-all",
                "tk.storeId",
                id,
            ],
            cwd,
        )
        .map_err(|_| ConfigError("replace"))?;
    if !out.succeeded() || pointers(runner, cwd)? != [id] {
        return Err(ConfigError("replace"));
    }
    Ok(())
}

/// Inspect the exact former Git Common Directory, without parent discovery.
pub fn former_pointers<R: ProcRunner + ?Sized>(
    runner: &R,
    common: &Path,
) -> Result<Vec<String>, ConfigError> {
    let common_text = common.to_str().ok_or(ConfigError("inspect ownership of"))?;
    let output = runner
        .run(
            &[
                "git",
                "--git-dir",
                common_text,
                "config",
                "--local",
                "--no-includes",
                "--null",
                "--get-all",
                "tk.storeId",
            ],
            common,
        )
        .map_err(|_| ConfigError("inspect ownership of"))?;
    decode(output)
}

/// Decode NUL-terminated local config results without exposing Git diagnostics.
fn values<R: ProcRunner + ?Sized>(
    runner: &R,
    cwd: &Path,
    query: &[&str],
) -> Result<Vec<String>, ConfigError> {
    let mut args = vec!["git", "config", "--local", "--no-includes", "--null"];
    args.extend_from_slice(query);
    let output = runner.run(&args, cwd).map_err(|_| ConfigError("read"))?;
    decode(output)
}

fn decode(output: crate::proc::RunOutput) -> Result<Vec<String>, ConfigError> {
    match output.exit_code {
        1 if output.stdout.is_empty() => Ok(Vec::new()),
        0 => {
            let text = String::from_utf8(output.stdout).map_err(|_| ConfigError("decode"))?;
            let Some(text) = text.strip_suffix('\0') else {
                return Err(ConfigError("decode"));
            };
            Ok(text.split('\0').map(str::to_owned).collect())
        }
        _ => Err(ConfigError("read")),
    }
}
