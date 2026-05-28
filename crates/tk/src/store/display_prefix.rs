//! Derives a Local Display ID prefix from a repository basename.
//!
//! Algorithm (per `ARCHITECTURE.md`, "Repository Store Contracts"):
//!
//! - Lowercase.
//! - Treat underscores as separators (so `my_cool_app` → `my-cool-app`).
//! - Split on separators and punctuation (`-`, `_`, `.`, `/`, `:`, `#`,
//!   whitespace).
//! - Drop empty segments.
//! - If the joined form (segments joined with `-`) is at most 12 chars, use it.
//! - Else if the first two segments joined with `-` fit in 12 chars, use that.
//! - Else truncate the concatenated sanitized basename to 12 chars.
//! - If the result is empty or starts with a digit, prefix with `tk-`.
//!
//! Vowels are not stripped. Output is always lowercase ASCII.

/// Maximum length of the stored local Display ID prefix.
pub const MAX_PREFIX_LEN: usize = 12;

const SEPARATORS: &[char] = &['-', '_', '.', '/', ':', '#', ' ', '\t'];

/// Derive a Display ID prefix from `basename`. Always returns lowercase ASCII.
#[must_use]
pub fn derive(basename: &str) -> String {
    let lowered = basename.to_ascii_lowercase();
    let segments: Vec<String> = lowered
        .split(|c| SEPARATORS.contains(&c))
        .filter_map(|raw| {
            let filtered: String = raw.chars().filter(char::is_ascii_alphanumeric).collect();
            if filtered.is_empty() {
                None
            } else {
                Some(filtered)
            }
        })
        .collect();

    let result = choose_shape(&segments);
    if result.is_empty() || result.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("tk-{result}");
    }
    result
}

fn choose_shape(segments: &[String]) -> String {
    let joined_all = segments.join("-");
    if !joined_all.is_empty() && joined_all.len() <= MAX_PREFIX_LEN {
        return joined_all;
    }
    if segments.len() >= 2 {
        let joined_two = segments[..2].join("-");
        if joined_two.len() <= MAX_PREFIX_LEN {
            return joined_two;
        }
    }
    let concat: String = segments.concat();
    let cap = concat.len().min(MAX_PREFIX_LEN);
    concat[..cap].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_single_word() {
        assert_eq!(derive("ticket"), "ticket");
    }

    #[test]
    fn lowercases() {
        assert_eq!(derive("Ticket"), "ticket");
    }

    #[test]
    fn multi_word_fits_joined() {
        assert_eq!(derive("my-cool-app"), "my-cool-app");
    }

    #[test]
    fn long_basename_falls_back_to_first_two_segments() {
        assert_eq!(derive("src-cafe-extras-and-more"), "src-cafe");
    }

    #[test]
    fn very_long_single_word_truncates_to_12() {
        assert_eq!(derive("abcdefghijklmnopqrstuvwxyz"), "abcdefghijkl");
    }

    #[test]
    fn underscores_treated_as_separators() {
        assert_eq!(derive("my_cool_app"), "my-cool-app");
    }

    #[test]
    fn punctuation_stripped_from_segments() {
        assert_eq!(derive("ab.cd!ef"), "ab-cdef");
    }

    #[test]
    fn empty_becomes_tk_dash() {
        assert_eq!(derive(""), "tk-");
    }

    #[test]
    fn all_punctuation_becomes_tk_dash() {
        assert_eq!(derive("---"), "tk-");
    }

    #[test]
    fn digit_leading_prefixed_with_tk_dash() {
        assert_eq!(derive("42-things"), "tk-42-things");
    }

    #[test]
    fn vowels_preserved() {
        assert_eq!(derive("iou"), "iou");
    }

    #[test]
    fn long_first_two_segments_truncate_full_basename() {
        assert_eq!(derive("alphabetone-betatwo-gamma"), "alphabetoneb");
    }
}
