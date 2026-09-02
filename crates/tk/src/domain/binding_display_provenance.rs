//! Provenance for the local Display ID displaced by an active Backend Binding.
//!
//! ADR-0047 requires Detach to restore exact identity and refuse ambiguous
//! legacy Alias history. The three variants mirror the Repository Store CHECK
//! constraint; [`BindingDisplayProvenance::text`] is the storage contract.

/// What the Repository Store knows about the local Display ID an active
/// Backend Binding displaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingDisplayProvenance {
    /// The Item entered through ordinary Adopt and displaced no local ID.
    None,
    /// Promotion displaced the local ID stored beside this value.
    Known,
    /// Legacy Alias history cannot identify one exact displaced local ID.
    Ambiguous,
}

impl BindingDisplayProvenance {
    /// SQLite storage spelling accepted by the `items` CHECK constraint.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Known => "known",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_matches_the_check_constrained_spellings() {
        assert_eq!(BindingDisplayProvenance::None.text(), "none");
        assert_eq!(BindingDisplayProvenance::Known.text(), "known");
        assert_eq!(BindingDisplayProvenance::Ambiguous.text(), "ambiguous");
    }
}
