//! Terminal-rendering sanitisers for Repository Store text fields.
//!
//! Stored titles and bodies remain byte-for-byte in the SQLite store.
//! These helpers sit at output boundaries so user/Remote-controlled text
//! cannot emit bytes that terminals interpret as SGR, OSC/APC, cursor
//! movement, bell, or line-editing controls — while still making those
//! bytes visible to developers (lowercase `\xNN` text).
//!
//! Two shapes:
//!
//! - [`write_sanitized_line`]: titles, summaries, and blocker reasons
//!   are single-line fields. CR / LF / Tab fold to spaces so a stray
//!   newline cannot rewrite surrounding row layout.
//! - [`write_sanitized_body`]: multi-line Ticket bodies. LF and Tab are
//!   layout-preserving; CRLF normalises to LF; bare CR escapes.
//!
//! Both write to a `&mut dyn io::Write` rather than allocating a
//! sanitised `String`, avoiding a copy for the all-clean common case.

use std::io;

#[derive(Clone, Copy)]
enum TextShape {
    Line,
    Body,
}

#[derive(Clone, Copy)]
enum Replacement {
    Clean,
    Space,
    Escape,
    Skip,
}

/// Write a single-line Repository Store field with terminal control
/// bytes rendered inert. CR / LF / Tab fold to spaces; other control
/// bytes render as lowercase `\xNN` text.
pub fn write_sanitized_line<W: io::Write + ?Sized>(writer: &mut W, text: &[u8]) -> io::Result<()> {
    write_sanitized(writer, text, TextShape::Line)
}

/// Write a multi-line Repository Store body with terminal control bytes
/// rendered inert. LF and Tab remain layout bytes for descriptions; CRLF
/// normalises to LF; bare CR and other control bytes render as lowercase
/// `\xNN` text.
pub fn write_sanitized_body<W: io::Write + ?Sized>(writer: &mut W, text: &[u8]) -> io::Result<()> {
    write_sanitized(writer, text, TextShape::Body)
}

fn write_sanitized<W: io::Write + ?Sized>(
    writer: &mut W,
    text: &[u8],
    shape: TextShape,
) -> io::Result<()> {
    // Chunked emission: defer writing until a non-`Clean` byte forces a
    // substitution so all-clean spans emit as a single `write_all`.
    // `clean_start` marks the start of the pending span; `peekable` supplies
    // the one-byte lookahead the Body shape needs to fold `\r\n` into `\n`.
    let mut clean_start = 0;
    let mut bytes = text.iter().copied().enumerate().peekable();
    while let Some((i, byte)) = bytes.next() {
        let next = bytes.peek().map(|&(_, b)| b);
        match classify(byte, next, shape) {
            Replacement::Clean => {}
            Replacement::Space => {
                writer.write_all(&text[clean_start..i])?;
                writer.write_all(b" ")?;
                clean_start = i + 1;
            }
            Replacement::Escape => {
                writer.write_all(&text[clean_start..i])?;
                write_hex(writer, byte)?;
                clean_start = i + 1;
            }
            Replacement::Skip => {
                writer.write_all(&text[clean_start..i])?;
                clean_start = i + 1;
            }
        }
    }
    writer.write_all(&text[clean_start..])
}

/// Classify one byte given the byte that follows it (`None` at end of input).
/// The `next` lookahead is what lets the Body shape drop the `\r` of a `\r\n`
/// pair while still escaping a bare `\r`.
fn classify(byte: u8, next: Option<u8>, shape: TextShape) -> Replacement {
    match shape {
        TextShape::Line => {
            if byte == b'\r' || byte == b'\n' || byte == b'\t' {
                Replacement::Space
            } else if is_control(byte) {
                Replacement::Escape
            } else {
                Replacement::Clean
            }
        }
        TextShape::Body => {
            if byte == b'\r' {
                if next == Some(b'\n') {
                    Replacement::Skip
                } else {
                    Replacement::Escape
                }
            } else if byte == b'\n' || byte == b'\t' {
                Replacement::Clean
            } else if is_control(byte) {
                Replacement::Escape
            } else {
                Replacement::Clean
            }
        }
    }
}

/// `true` if `byte` is an ASCII (C0) or C1 control. C1 controls
/// (`0x80..=0x9F`) include `0x9B`, the 8-bit form of CSI that an xterm
/// or VTE-derivative in 8-bit input mode interprets as the SGR
/// introducer. `u8::is_ascii_control` only covers C0 (`0x00..=0x1F`)
/// and DEL (`0x7F`); reaching for it alone would leave 8-bit CSI / DCS
/// / OSC / PM / APC through unescaped.
fn is_control(byte: u8) -> bool {
    byte.is_ascii_control() || (0x80..=0x9F).contains(&byte)
}

fn write_hex<W: io::Write + ?Sized>(writer: &mut W, byte: u8) -> io::Result<()> {
    write!(writer, "\\x{byte:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_folds_whitespace_and_escapes_controls() {
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_line(&mut buf, b"Hello\r\n\tWorld!\x1b[31mBold\x07").unwrap();
        assert_eq!(buf, b"Hello   World!\\x1b[31mBold\\x07");
    }

    #[test]
    fn body_preserves_layout_whitespace_and_escapes_controls() {
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_body(&mut buf, b"Line 1\r\n\tLine 2\x1b[31mRed\x7f\rStandalone").unwrap();
        assert_eq!(buf, b"Line 1\n\tLine 2\\x1b[31mRed\\x7f\\x0dStandalone");
    }

    #[test]
    fn line_writes_clean_text_unchanged() {
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_line(&mut buf, b"plain title").unwrap();
        assert_eq!(buf, b"plain title");
    }

    #[test]
    fn body_normalises_crlf_to_lf() {
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_body(&mut buf, b"a\r\nb\r\nc").unwrap();
        assert_eq!(buf, b"a\nb\nc");
    }

    #[test]
    fn body_escapes_bare_cr() {
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_body(&mut buf, b"x\ry").unwrap();
        assert_eq!(buf, b"x\\x0dy");
    }

    #[test]
    fn line_escapes_high_control_del() {
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_line(&mut buf, b"a\x7fb").unwrap();
        assert_eq!(buf, b"a\\x7fb");
    }

    #[test]
    fn line_escapes_c1_control_csi() {
        // 0x9B is 8-bit CSI; xterm and VTE in 8-bit input mode parse it
        // as ESC '[' and treat the following bytes as an SGR sequence.
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_line(&mut buf, b"a\x9b31mb").unwrap();
        assert_eq!(buf, b"a\\x9b31mb");
    }

    #[test]
    fn body_escapes_c1_control_csi() {
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_body(&mut buf, b"a\x9b31mb").unwrap();
        assert_eq!(buf, b"a\\x9b31mb");
    }
}
