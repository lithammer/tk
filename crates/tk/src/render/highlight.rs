//! Match highlighting for `tk grep` (ADR-0026).
//!
//! Writes one sanitized line with every non-overlapping regex match wrapped in
//! the [`palette::MATCH`] style. Highlighting and sanitisation are *interleaved*
//! rather than sequential: the matched text is user/Remote-controlled, so it is
//! sanitised like every other span — the SGR open/close only bracket the
//! already-inert bytes. Under [`ColorChoice::Never`](super::styler::ColorChoice)
//! the open/close render empty, so the output is byte-identical to a plain
//! [`sanitize::write_sanitized_line`].

use std::io::Write;

use regex::Regex;

use crate::render::palette;
use crate::render::sanitize;
use crate::render::styler::SubStyler;

/// Write `text` as a single sanitized line, wrapping each non-overlapping match
/// of `re` in [`palette::MATCH`]. Zero-width matches are not highlighted (a
/// `MATCH` span around no bytes would be noise) but still split the surrounding
/// spans correctly.
pub fn write_highlighted_line<W: Write + ?Sized>(
    stdout: &mut W,
    text: &str,
    re: &Regex,
    styler: SubStyler,
) -> std::io::Result<()> {
    // Regex match offsets are valid byte offsets into `text`, so slice the byte
    // view directly (avoids clippy's str-slice-then-as_bytes panic concern).
    let bytes = text.as_bytes();
    let mut last = 0;
    for m in re.find_iter(text) {
        if m.start() == m.end() {
            continue;
        }
        sanitize::write_sanitized_line(stdout, &bytes[last..m.start()])?;
        write!(stdout, "{}", styler.open(palette::MATCH))?;
        sanitize::write_sanitized_line(stdout, &bytes[m.start()..m.end()])?;
        write!(stdout, "{}", styler.close(palette::MATCH))?;
        last = m.end();
    }
    sanitize::write_sanitized_line(stdout, &bytes[last..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Styler;

    fn highlight(text: &str, pattern: &str, styler: Styler) -> String {
        let re = Regex::new(pattern).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        write_highlighted_line(&mut buf, text, &re, styler.for_stdout()).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn wraps_matches_in_bright_yellow_under_color() {
        // Bright yellow foreground opens `\x1b[93m` and closes `\x1b[39m`.
        let out = highlight("the auth token", "auth", Styler::always());
        assert_eq!(out, "the \u{1b}[93mauth\u{1b}[39m token");
    }

    #[test]
    fn plain_output_is_byte_identical_to_a_sanitized_line() {
        let out = highlight("the auth token", "auth", Styler::plain());
        assert_eq!(out, "the auth token");
    }

    #[test]
    fn highlights_every_non_overlapping_match() {
        let out = highlight("auth and auth", "auth", Styler::always());
        assert_eq!(out, "\u{1b}[93mauth\u{1b}[39m and \u{1b}[93mauth\u{1b}[39m");
    }

    #[test]
    fn zero_width_match_is_not_highlighted() {
        // `x*` matches empty positions; none should emit a MATCH span, and the
        // text passes through plain.
        let out = highlight("abc", "x*", Styler::always());
        assert_eq!(out, "abc");
    }
}
