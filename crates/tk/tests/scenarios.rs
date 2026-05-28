//! Subprocess-driven txtar scenario harness.
//!
//! Slice-0 differential oracle, designed in ADR-0018:
//!
//! - Reads `.txtar` files from `src/testing/scenarios/` (the same data files
//!   the Zig oracle's in-process `script.zig` runner consumes).
//! - Materialises a per-scenario `$WORK` tempdir and any `input/` sections.
//! - Executes the script section line-by-line: `mkdir`, `cd`, `stdin`, real
//!   `git`, and the built Rust `tk` binary.
//! - Compares aggregated stdout, stderr, and the final-command exit code
//!   byte-exact against the `expected/{stdout,stderr,exit}` sections, after
//!   substituting the tmpdir path with `$WORK`.
//!
//! The hand-rolled parser is intentionally small: slice 0 ships one scenario
//! (`init/init_fresh.txtar`), and a tighter loop reads better than a
//! third-party crate's API surface for a one-file harness. If the runner
//! grows past ~500 LOC the trade-off flips and we revisit.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;

#[test]
fn init_fresh_scenario_passes_byte_exact() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/tk -> repo root is two levels up.
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf();
    let fixture = repo_root.join("src/testing/scenarios/init/init_fresh.txtar");
    run_scenario(&fixture);
}

// ---- Driver -------------------------------------------------------------

fn run_scenario(fixture_path: &Path) {
    let data = fs::read_to_string(fixture_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", fixture_path.display()));
    let sections = parse_txtar(&data);

    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().canonicalize().expect("canonicalize tmp path");

    materialize_inputs(&work, &sections);
    let result = execute_script(&work, &sections);

    let expected_stdout = section(&sections, "expected/stdout")
        .unwrap_or(&[])
        .to_vec();
    let expected_stderr = section(&sections, "expected/stderr")
        .unwrap_or(&[])
        .to_vec();
    let expected_exit: i32 = std::str::from_utf8(section(&sections, "expected/exit").unwrap_or(b"0\n"))
        .expect("expected/exit must be UTF-8")
        .trim()
        .parse()
        .expect("expected/exit is an integer");

    let actual_stdout = normalize_work(&result.stdout, &work);
    let actual_stderr = normalize_work(&result.stderr, &work);

    if actual_stdout != expected_stdout
        || actual_stderr != expected_stderr
        || result.final_exit != expected_exit
    {
        eprintln!(
            "--- stdout expected ---\n{}",
            String::from_utf8_lossy(&expected_stdout)
        );
        eprintln!(
            "--- stdout actual ---\n{}",
            String::from_utf8_lossy(&actual_stdout)
        );
        eprintln!(
            "--- stderr expected ---\n{}",
            String::from_utf8_lossy(&expected_stderr)
        );
        eprintln!(
            "--- stderr actual ---\n{}",
            String::from_utf8_lossy(&actual_stderr)
        );
        eprintln!(
            "--- exit expected: {expected_exit}, actual: {} ---",
            result.final_exit
        );
        panic!(
            "scenario {} failed byte-exact comparison",
            fixture_path.display()
        );
    }
}

struct ScriptResult {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    final_exit: i32,
}

/// Internal short-circuit: a `script:` error mirrors the Zig oracle's
/// `appendScriptError`: write a `script: …` line to stderr, set exit 3,
/// and stop executing further script lines. Tests can then assert against
/// the byte-exact `script: …` shape just as the Zig harness does.
fn script_error(result: &mut ScriptResult, msg: &str) {
    result.stderr.extend_from_slice(b"script: ");
    result.stderr.extend_from_slice(msg.as_bytes());
    result.stderr.push(b'\n');
    result.final_exit = 3;
}

fn execute_script(work: &Path, sections: &[Section]) -> ScriptResult {
    let script_bytes = section(sections, "script").unwrap_or(&[]);
    let script = std::str::from_utf8(script_bytes).expect("script section must be UTF-8");
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("WORK".into(), work.to_string_lossy().into_owned());

    let mut active_cwd = work.to_path_buf();
    let mut pending_stdin: Option<Vec<u8>> = None;
    let mut result = ScriptResult {
        stdout: Vec::new(),
        stderr: Vec::new(),
        final_exit: 0,
    };

    for line in script.lines() {
        let argv = tokenize_line(line, &env);
        if argv.is_empty() {
            continue;
        }
        match argv[0].as_str() {
            "mkdir" => {
                if argv.len() != 2 {
                    script_error(&mut result, "mkdir requires exactly one path");
                    return result;
                }
                if !is_valid_input_path(&argv[1]) {
                    script_error(
                        &mut result,
                        &format!("invalid mkdir path: {}", argv[1]),
                    );
                    return result;
                }
                if let Err(e) = fs::create_dir_all(active_cwd.join(&argv[1])) {
                    script_error(&mut result, &format!("mkdir failed: {}: {e}", argv[1]));
                    return result;
                }
            }
            "cd" => {
                if argv.len() != 2 {
                    script_error(&mut result, "cd requires exactly one path");
                    return result;
                }
                if !is_valid_input_path(&argv[1]) {
                    script_error(&mut result, &format!("invalid cd path: {}", argv[1]));
                    return result;
                }
                match active_cwd.join(&argv[1]).canonicalize() {
                    Ok(next) => active_cwd = next,
                    Err(e) => {
                        script_error(&mut result, &format!("cd failed: {}: {e}", argv[1]));
                        return result;
                    }
                }
            }
            "stdin" => {
                if argv.len() != 2 {
                    script_error(&mut result, "stdin requires exactly one source");
                    return result;
                }
                if pending_stdin.is_some() {
                    script_error(&mut result, "stdin already set");
                    return result;
                }
                let bytes = match argv[1].as_str() {
                    "stdout" => result.stdout.clone(),
                    "stderr" => result.stderr.clone(),
                    rel => {
                        if !is_valid_input_path(rel) {
                            script_error(&mut result, &format!("invalid stdin source: {rel}"));
                            return result;
                        }
                        match fs::read(active_cwd.join(rel)) {
                            Ok(b) => b,
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                script_error(
                                    &mut result,
                                    &format!("stdin source not found: {rel}"),
                                );
                                return result;
                            }
                            Err(e) => {
                                script_error(
                                    &mut result,
                                    &format!("stdin source read failed: {rel}: {e}"),
                                );
                                return result;
                            }
                        }
                    }
                };
                pending_stdin = Some(bytes);
            }
            "git" => {
                let out = match Command::new("git")
                    .args(&argv[1..])
                    .current_dir(&active_cwd)
                    .output()
                {
                    Ok(o) => o,
                    Err(e) => {
                        script_error(&mut result, &format!("git failed to run: {e}"));
                        return result;
                    }
                };
                result.final_exit = out.status.code().unwrap_or(255);
                result.stdout.extend_from_slice(&out.stdout);
                result.stderr.extend_from_slice(&out.stderr);
            }
            "tk" => {
                let mut cmd = Command::cargo_bin("tk").expect("cargo bin tk");
                cmd.args(&argv[1..]).current_dir(&active_cwd);
                // Force a deterministic clock so any `applied_at` value the
                // command emits is reproducible. Scenarios that need a
                // specific stamp can set TK_NOW themselves via env once we
                // grow that script directive; slice 0 hard-codes a value
                // that doesn't appear in `tk init` stdout/stderr but does
                // land in `schema_migrations.applied_at`.
                cmd.env("TK_NOW", "2026-05-09T00:00:00.000Z");
                cmd.env("TK_RAND_SEED", "0");
                let out = if let Some(input) = pending_stdin.take() {
                    use std::io::Write;
                    use std::process::Stdio;
                    cmd.stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped());
                    let mut child = cmd.spawn().expect("spawn tk");
                    child
                        .stdin
                        .as_mut()
                        .expect("tk stdin pipe")
                        .write_all(&input)
                        .expect("tk stdin write");
                    child.wait_with_output().expect("tk wait")
                } else {
                    cmd.output().expect("run tk")
                };
                result.final_exit = out.status.code().unwrap_or(255);
                result.stdout.extend_from_slice(&out.stdout);
                result.stderr.extend_from_slice(&out.stderr);
            }
            _ => {
                // Unknown directives are no-ops; the Zig oracle has the same
                // tolerant stance so a `# comment` line dropped to a bare
                // word never trips a real failure.
            }
        }
    }
    if pending_stdin.is_some() {
        script_error(&mut result, "stdin set but no tk command consumed it");
    }
    result
}

fn materialize_inputs(work: &Path, sections: &[Section]) {
    for sec in sections {
        let Some(rel) = sec.name.strip_prefix("input/") else {
            continue;
        };
        assert!(
            is_valid_input_path(rel),
            "input section path escapes $WORK: {rel}"
        );
        let abs = work.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("mkdir input parent");
        }
        fs::write(&abs, &sec.body).expect("write input section");
    }
}

/// Substitute the work directory with `$WORK`, then on Windows rewrite native
/// `\` separators inside `$WORK\…` spans to `/` (mirrors the Zig oracle's
/// `normalizeWorkSpans` in `src/testing/script.zig`). Forward-slash output is
/// the txtar convention; without this rewrite the same fixture cannot pass on
/// both POSIX and Windows.
fn normalize_work(bytes: &[u8], work: &Path) -> Vec<u8> {
    let work_str = work.to_string_lossy();
    let work_bytes = work_str.as_bytes();
    if work_bytes.is_empty() {
        return bytes.to_vec();
    }
    // Substring replace over bytes — preserves non-UTF-8 fragments verbatim,
    // unlike `String::replace`.
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(work_bytes) {
            out.extend_from_slice(b"$WORK");
            i += work_bytes.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    if cfg!(target_os = "windows") {
        normalize_work_spans(&mut out);
    }
    out
}

/// Rewrite `\` to `/` inside each `$WORK`-prefixed path token. Mutates in
/// place. Two guards keep it surgical: (1) the byte after `$WORK` must be a
/// path separator or non-path terminator (so `$WORKSPACE` is left alone);
/// (2) walking stops at the first non-path byte (so backslashes in quoted
/// literals on the same line aren't collateral-damaged).
fn normalize_work_spans(buf: &mut [u8]) {
    const MARKER: &[u8] = b"$WORK";
    let mut i = 0;
    while i + MARKER.len() <= buf.len() {
        if &buf[i..i + MARKER.len()] != MARKER {
            i += 1;
            continue;
        }
        let after = i + MARKER.len();
        if !is_work_boundary(buf, after) {
            i = after;
            continue;
        }
        let mut j = after;
        while j < buf.len() && is_path_byte(buf[j]) {
            if buf[j] == b'\\' {
                buf[j] = b'/';
            }
            j += 1;
        }
        i = j;
    }
}

fn is_work_boundary(buf: &[u8], after: usize) -> bool {
    if after == buf.len() {
        return true;
    }
    let c = buf[after];
    c == b'/' || c == b'\\' || !is_path_byte(c)
}

fn is_path_byte(c: u8) -> bool {
    matches!(c,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'
        | b'/' | b'\\' | b'.' | b'_' | b'-' | b':' | b'~')
}

fn is_valid_input_path(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') {
        return false;
    }
    !rel.split('/').any(|segment| segment == "..")
}

// ---- txtar parsing ------------------------------------------------------

struct Section {
    name: String,
    body: Vec<u8>,
}

fn parse_txtar(data: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut current_name: Option<String> = Some(String::new()); // prelude
    let mut current_body: Vec<u8> = Vec::new();
    for line in data.split_inclusive('\n') {
        let trimmed = line
            .trim_end_matches(['\r', ' ', '\t'])
            .trim_end_matches('\n');
        if let Some(name) = section_header(trimmed) {
            if let Some(prev) = current_name.take() {
                if !prev.is_empty() || !current_body.is_empty() {
                    out.push(Section {
                        name: prev,
                        body: std::mem::take(&mut current_body),
                    });
                }
            }
            current_name = Some(name.to_string());
            current_body.clear();
        } else {
            current_body.extend_from_slice(line.as_bytes());
        }
    }
    if let Some(name) = current_name {
        if !name.is_empty() || !current_body.is_empty() {
            out.push(Section {
                name,
                body: current_body,
            });
        }
    }
    out
}

fn section_header(line: &str) -> Option<&str> {
    if line.len() < 7 {
        return None;
    }
    let line = line.strip_prefix("-- ")?;
    line.strip_suffix(" --")
}

fn section<'a>(sections: &'a [Section], name: &str) -> Option<&'a [u8]> {
    sections
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.body.as_slice())
}

// ---- Script tokenizer ---------------------------------------------------

/// Minimal port of `src/testing/script.zig` `tokenizeLine`:
/// whitespace splits args, single quotes preserve literals (`''` produces a
/// literal `'`), `$NAME` / `${NAME}` expand from `env` (undefined names are
/// preserved verbatim with their `$`), and `#` outside quotes starts a comment.
///
/// Each token is pushed unconditionally once the inner loop runs at least one
/// step, matching the Zig oracle (`src/testing/script.zig:112`). Empty quoted
/// tokens (`''`) and empty-`$VAR` expansions both yield real positional empty
/// strings.
fn tokenize_line(line: &str, env: &HashMap<String, String>) -> Vec<String> {
    let mut chars = line.chars().peekable();
    let mut tokens: Vec<String> = Vec::new();
    while let Some(&c) = chars.peek() {
        if c == ' ' || c == '\t' || c == '\r' {
            chars.next();
            continue;
        }
        if c == '#' {
            break;
        }
        let mut token = String::new();
        while let Some(&ch) = chars.peek() {
            if ch == ' ' || ch == '\t' || ch == '\r' || ch == '#' {
                break;
            }
            if ch == '\'' {
                chars.next();
                while let Some(qc) = chars.next() {
                    if qc == '\'' {
                        // `''` inside a single-quoted span is the literal `'`.
                        if chars.peek() == Some(&'\'') {
                            token.push('\'');
                            chars.next();
                        } else {
                            break;
                        }
                    } else {
                        token.push(qc);
                    }
                }
                continue;
            }
            if ch == '$' {
                chars.next();
                let mut name = String::new();
                if chars.peek() == Some(&'{') {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        if nc == '}' {
                            chars.next();
                            break;
                        }
                        name.push(nc);
                        chars.next();
                    }
                } else {
                    while let Some(&nc) = chars.peek() {
                        if !nc.is_ascii_alphanumeric() && nc != '_' {
                            break;
                        }
                        name.push(nc);
                        chars.next();
                    }
                }
                if let Some(val) = env.get(&name) {
                    token.push_str(val);
                } else {
                    token.push('$');
                    token.push_str(&name);
                }
                continue;
            }
            token.push(ch);
            chars.next();
        }
        // The outer loop only enters this point when `c` is non-whitespace
        // and non-`#`, so we've always consumed at least one input char that
        // resolved to "token entry" (a literal char, a quoted span, or a
        // `$VAR` expansion). Push unconditionally to mirror Zig.
        tokens.push(token);
    }
    tokens
}
