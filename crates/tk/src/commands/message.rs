//! Git-commit-style message parsing for `tk add` and `tk update`.
//!
//! The first paragraph becomes the title (joined with single spaces when
//! it spans multiple lines); subsequent paragraphs become the body
//! (preserved verbatim after outer blank-line trimming). Line endings
//! normalise CRLF → LF and bare CR → LF before splitting so messages
//! authored on any platform produce the same parsed form.

use std::io::Read;
use std::path::Path;

use thiserror::Error;

/// Parsed Ticket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMessage {
    pub title: String,
    pub body: String,
}

/// Errors returned by [`parse`] / [`parse_from_paragraphs`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// All lines were blank — the title is required.
    #[error("message is empty")]
    Empty,
    /// Embedded NUL byte; refuse rather than truncate.
    #[error("message contains a NUL byte")]
    NulByte,
}

/// Parse raw message bytes into title/body.
pub fn parse(raw: &str) -> Result<ParsedMessage, ParseError> {
    if raw.bytes().any(|b| b == 0) {
        return Err(ParseError::NulByte);
    }
    let normalized = normalize_line_endings(raw);
    let lines: Vec<&str> = normalized.lines().collect();

    let first = first_non_blank(&lines).ok_or(ParseError::Empty)?;
    let last = last_non_blank(&lines).expect("first_non_blank found one, so last exists");

    let mut title_end = first;
    while title_end <= last && !is_blank(lines[title_end]) {
        title_end += 1;
    }

    let mut title = String::new();
    for (i, line) in lines[first..title_end].iter().enumerate() {
        if i > 0 {
            title.push(' ');
        }
        title.push_str(line.trim_matches([' ', '\t']));
    }
    if title.is_empty() {
        return Err(ParseError::Empty);
    }

    let mut body_start = title_end;
    while body_start <= last && is_blank(lines[body_start]) {
        body_start += 1;
    }
    let body = if body_start <= last {
        join_body_lines(&lines[body_start..=last])
    } else {
        String::new()
    };

    Ok(ParsedMessage { title, body })
}

/// Parse paragraphs from repeated `-m` flags by joining them with double
/// newlines and feeding the result to [`parse`].
pub fn parse_from_paragraphs(paragraphs: &[String]) -> Result<ParsedMessage, ParseError> {
    if paragraphs.is_empty() {
        return Err(ParseError::Empty);
    }
    let combined = paragraphs.join("\n\n");
    parse(&combined)
}

fn normalize_line_endings(raw: &str) -> String {
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_blank(line: &str) -> bool {
    line.trim_matches([' ', '\t']).is_empty()
}

fn first_non_blank(lines: &[&str]) -> Option<usize> {
    lines.iter().position(|l| !is_blank(l))
}

fn last_non_blank(lines: &[&str]) -> Option<usize> {
    lines.iter().rposition(|l| !is_blank(l))
}

fn join_body_lines(lines: &[&str]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

/// Source of the message body — repeated `-m` flags, a file path, or
/// stdin (path `"-"`).
#[derive(Debug)]
pub enum Input<'a> {
    Paragraphs(&'a [String]),
    File(&'a str),
}

/// Failure of [`read_input`].
#[derive(Debug, Error)]
pub enum ReadError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("failed to read message file '{path}': {source}")]
    File {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to read message from stdin: {0}")]
    Stdin(std::io::Error),
}

/// Load and parse a command's message input.
pub fn read_input<R: Read + ?Sized>(
    input: Input<'_>,
    cwd: &Path,
    stdin: &mut R,
) -> Result<ParsedMessage, ReadError> {
    match input {
        Input::Paragraphs(paragraphs) => parse_from_paragraphs(paragraphs).map_err(Into::into),
        Input::File("-") => {
            let mut buf = String::new();
            stdin.read_to_string(&mut buf).map_err(ReadError::Stdin)?;
            parse(&buf).map_err(Into::into)
        }
        Input::File(path) => {
            let resolved = cwd.join(path);
            let raw = std::fs::read_to_string(&resolved).map_err(|source| ReadError::File {
                path: path.to_owned(),
                source,
            })?;
            parse(&raw).map_err(Into::into)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_multi_line_title_and_preserves_body() {
        let parsed = parse(
            "\n  First title line  \nSecond title line\n\nBody line one  \n\n\nBody line two\t\n\n",
        )
        .unwrap();
        assert_eq!(parsed.title, "First title line Second title line");
        assert_eq!(parsed.body, "Body line one  \n\n\nBody line two\t");
    }

    #[test]
    fn empty_input_is_rejected() {
        assert_eq!(parse("").unwrap_err(), ParseError::Empty);
        assert_eq!(parse("\n\n\t  \n").unwrap_err(), ParseError::Empty);
    }

    #[test]
    fn nul_byte_is_rejected() {
        assert_eq!(parse("title\0body").unwrap_err(), ParseError::NulByte);
    }

    #[test]
    fn crlf_normalises_to_lf() {
        let parsed = parse("Title\r\n\r\nBody line 1\r\nBody line 2\r\n").unwrap();
        assert_eq!(parsed.title, "Title");
        assert_eq!(parsed.body, "Body line 1\nBody line 2");
    }

    #[test]
    fn paragraphs_join_with_double_newline() {
        let parsed =
            parse_from_paragraphs(&["Title".into(), "Body p1".into(), "Body p2".into()]).unwrap();
        assert_eq!(parsed.title, "Title");
        assert_eq!(parsed.body, "Body p1\n\nBody p2");
    }

    #[test]
    fn paragraphs_empty_slice_returns_empty() {
        let err = parse_from_paragraphs(&[]).unwrap_err();
        assert_eq!(err, ParseError::Empty);
    }

    #[test]
    fn body_only_is_an_error_when_title_blank() {
        // Title paragraph must contain non-blank content; leading blank
        // lines are skipped to find the first paragraph, so a non-blank
        // body becomes the title here.
        let parsed = parse("\n\nBody as title").unwrap();
        assert_eq!(parsed.title, "Body as title");
        assert_eq!(parsed.body, "");
    }

    #[test]
    fn read_input_reads_stdin_for_dash_path() {
        let mut stdin = std::io::Cursor::new(b"From stdin\n\nBody".to_vec());
        let parsed = read_input(Input::File("-"), std::path::Path::new("/"), &mut stdin).unwrap();
        assert_eq!(parsed.title, "From stdin");
        assert_eq!(parsed.body, "Body");
    }
}
