//! The staged-filename shape shared by the atomic stage-and-rename installs in
//! `tk self-update` and `tk manpage --install`.
//!
//! Both commands write a temporary file beside their target and rename it into
//! place, so both need a name that cannot collide with a concurrent stager's.

use rand::Rng;

/// Build a staged filename: `prefix` followed by 64 random bits as lowercase
/// hex. The randomness is what lets two stagers writing into the same target
/// directory run at once without pid sniffing.
pub(crate) fn staged_file_name<R: Rng + ?Sized>(prefix: &str, rng: &mut R) -> String {
    let mut bytes = [0u8; 8];
    rng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(prefix.len() + 16);
    s.push_str(prefix);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn staged_name_is_the_prefix_plus_sixteen_hex_digits() {
        let mut rng = StdRng::seed_from_u64(0);
        let name = staged_file_name(".tk.tmp.", &mut rng);
        assert!(name.starts_with(".tk.tmp."), "{name}");
        let suffix = &name[".tk.tmp.".len()..];
        assert_eq!(suffix.len(), 16, "64 random bits, hex-encoded");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{suffix}");
    }
}
