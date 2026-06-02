//! Terminal-rendering sanitisers for Repository Store text fields.
//!
//! Stored titles and bodies remain byte-for-byte in the SQLite store.
//! These helpers sit at output boundaries so user/Remote-controlled text
//! cannot emit bytes that terminals interpret as SGR, OSC/APC, cursor
//! movement, bell, or line-editing controls — while still making those
//! bytes visible to developers (lowercase `\xNN` text).
//!
//! Sanitising is UTF-8 aware: the input is decoded as UTF-8 and classified
//! per `char`, so legitimate multibyte characters (arrows, dashes, accented
//! letters) pass through untouched. Only genuine control characters —
//! including the C1 block (`U+0080..=U+009F`), which `char::is_control`
//! covers — are escaped. Bytes that are not valid UTF-8 cannot be characters,
//! so each is escaped individually. This neutralises 8-bit CSI both as a bare
//! `0x9B` byte (invalid UTF-8) and as the encoded codepoint `U+009B`
//! (`0xC2 0x9B`), while no longer mangling the continuation bytes
//! (`0x80..=0x9F`) of valid multibyte characters.
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
    // Decode as UTF-8 so continuation bytes (`0x80..=0x9F`) of valid multibyte
    // characters are not mistaken for C1 controls. Each chunk is a maximal run
    // of valid UTF-8 followed by an invalid byte sequence; the valid run is
    // classified per `char`, the invalid bytes are escaped individually. `\r\n`
    // is pure ASCII, so a CRLF pair always lands within one valid run and the
    // Body shape's lookahead never straddles a chunk boundary.
    for chunk in text.utf8_chunks() {
        write_sanitized_str(writer, chunk.valid(), shape)?;
        for &byte in chunk.invalid() {
            write_hex(writer, byte)?;
        }
    }
    Ok(())
}

fn write_sanitized_str<W: io::Write + ?Sized>(
    writer: &mut W,
    text: &str,
    shape: TextShape,
) -> io::Result<()> {
    // Chunked emission: defer writing until a non-`Clean` char forces a
    // substitution so all-clean spans emit as a single `write_all`.
    // `clean_start` marks the start of the pending span; `peekable` supplies
    // the one-char lookahead the Body shape needs to fold `\r\n` into `\n`.
    let bytes = text.as_bytes();
    let mut clean_start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        let next = chars.peek().map(|&(_, c)| c);
        match classify(ch, next, shape) {
            Replacement::Clean => {}
            Replacement::Space => {
                writer.write_all(&bytes[clean_start..i])?;
                writer.write_all(b" ")?;
                clean_start = i + ch.len_utf8();
            }
            Replacement::Escape => {
                writer.write_all(&bytes[clean_start..i])?;
                write_char_escape(writer, ch)?;
                clean_start = i + ch.len_utf8();
            }
            Replacement::Skip => {
                writer.write_all(&bytes[clean_start..i])?;
                clean_start = i + ch.len_utf8();
            }
        }
    }
    writer.write_all(&bytes[clean_start..])
}

/// Classify one char given the char that follows it (`None` at end of input).
/// The `next` lookahead is what lets the Body shape drop the `\r` of a `\r\n`
/// pair while still escaping a bare `\r`.
///
/// `char::is_control` covers C0 (`U+0000..=U+001F`), DEL (`U+007F`), and the
/// C1 block (`U+0080..=U+009F`). C1 includes `U+009B`, the CSI introducer an
/// xterm or VTE-derivative in 8-bit input mode parses as `ESC [`; escaping it
/// here keeps a UTF-8-encoded C1 control from reaching the terminal as SGR /
/// DCS / OSC / PM / APC.
fn classify(ch: char, next: Option<char>, shape: TextShape) -> Replacement {
    match shape {
        TextShape::Line => {
            if ch == '\r' || ch == '\n' || ch == '\t' {
                Replacement::Space
            } else if ch.is_control() {
                Replacement::Escape
            } else {
                Replacement::Clean
            }
        }
        TextShape::Body => {
            if ch == '\r' {
                if next == Some('\n') {
                    Replacement::Skip
                } else {
                    Replacement::Escape
                }
            } else if ch == '\n' || ch == '\t' {
                Replacement::Clean
            } else if ch.is_control() {
                Replacement::Escape
            } else {
                Replacement::Clean
            }
        }
    }
}

/// Escape a control char as lowercase `\xNN`. Every char reaching this point is
/// a C0, DEL, or C1 control, so its codepoint fits in a single byte and shares
/// the byte-escape spelling.
fn write_char_escape<W: io::Write + ?Sized>(writer: &mut W, ch: char) -> io::Result<()> {
    write_hex(writer, ch as u8)
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

    #[test]
    fn body_preserves_multibyte_utf8() {
        // Regression: the continuation bytes of these characters fall in
        // `0x80..=0x9F` (→ E2 86 92, … E2 80 A6, — E2 80 94, ↔ E2 86 94). A
        // byte-based C1 escape mangled each into `�\x..\x..`; UTF-8-aware
        // classification must pass them through verbatim.
        let mut buf: Vec<u8> = Vec::new();
        let input = "open_for_command → open_existing — … ↔";
        write_sanitized_body(&mut buf, input.as_bytes()).unwrap();
        assert_eq!(buf, input.as_bytes());
    }

    #[test]
    fn line_preserves_multibyte_utf8() {
        let mut buf: Vec<u8> = Vec::new();
        let input = "café → résumé";
        write_sanitized_line(&mut buf, input.as_bytes()).unwrap();
        assert_eq!(buf, input.as_bytes());
    }

    #[test]
    fn body_escapes_utf8_encoded_c1_control() {
        // A genuine C1 control encoded as valid UTF-8 (U+009B = 0xC2 0x9B)
        // must still be neutralised, not passed through as 8-bit CSI.
        let mut buf: Vec<u8> = Vec::new();
        write_sanitized_body(&mut buf, "a\u{9b}31mb".as_bytes()).unwrap();
        assert_eq!(buf, b"a\\x9b31mb");
    }
}
