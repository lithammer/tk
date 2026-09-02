//! Provenance for the local Display ID displaced by an active Backend Binding.
//!
//! ADR-0047 requires Detach to restore exact identity and refuse ambiguous
//! legacy Alias history. This type joins the two constrained Repository Store
//! columns so invalid pairs cannot flow past the Store boundary.

use thiserror::Error;

/// What the Repository Store knows about the local Display ID an active
/// Backend Binding displaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingDisplayProvenance {
    /// The Item entered through ordinary Adopt and displaced no local ID.
    None,
    /// Promotion displaced the local ID stored beside this value.
    Known(String),
    /// Legacy Alias history cannot identify one exact displaced local ID.
    Ambiguous,
}

impl BindingDisplayProvenance {
    /// Paired SQLite values accepted by the `items` CHECK constraint.
    #[must_use]
    pub fn stored_values(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::None => ("none", None),
            Self::Known(display_id) => ("known", Some(display_id)),
            Self::Ambiguous => ("ambiguous", None),
        }
    }

    /// Join the two Repository Store columns into one valid domain value.
    pub fn from_stored(
        provenance: &str,
        local_display_id: Option<String>,
    ) -> Result<Self, InvalidBindingDisplayProvenance> {
        match (provenance, local_display_id) {
            ("none", None) => Ok(Self::None),
            ("known", Some(display_id)) => Ok(Self::Known(display_id)),
            ("ambiguous", None) => Ok(Self::Ambiguous),
            (provenance, _) => Err(InvalidBindingDisplayProvenance(provenance.into())),
        }
    }
}

/// A Repository Store row whose provenance columns violate their shared CHECK.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid Binding Display provenance `{0}`")]
pub struct InvalidBindingDisplayProvenance(String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_values_match_the_check_constrained_pairs() {
        assert_eq!(
            BindingDisplayProvenance::None.stored_values(),
            ("none", None)
        );
        assert_eq!(
            BindingDisplayProvenance::Known("tk-1".into()).stored_values(),
            ("known", Some("tk-1"))
        );
        assert_eq!(
            BindingDisplayProvenance::Ambiguous.stored_values(),
            ("ambiguous", None)
        );
    }

    #[test]
    fn stored_columns_form_one_domain_value() {
        assert_eq!(
            BindingDisplayProvenance::from_stored("known", Some("tk-1".into())),
            Ok(BindingDisplayProvenance::Known("tk-1".into()))
        );
        assert!(BindingDisplayProvenance::from_stored("known", None).is_err());
        assert!(BindingDisplayProvenance::from_stored("ambiguous", Some("tk-1".into())).is_err());
    }
}
