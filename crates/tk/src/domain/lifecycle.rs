//! Lifecycle: the Backend-shared axis ADR-0043 narrows `items.status` to.
//!
//! Two-valued, and the only axis a Backend Adapter observes or changes. Its
//! counterpart is the local-only [`crate::domain::work_state`]; ADR-0043
//! records how Item Status derives from the pair.

use serde::{Deserialize, Serialize};

/// The Backend-shared lifecycle of a Ticket or Epic: open or done.
///
/// `Lifecycle::Open` is the default for newly-created local work; Backend
/// intake names the imported value explicitly.
///
/// `Serialize`/`Deserialize` use `snake_case` (matching [`FailureClass`],
/// this codebase's other domain-enum serde precedent), with no catch-all
/// variant: `Lifecycle` is also `FromSql`/`ToSql` for `items.status`, where a
/// CHECK constraint pins exactly `open`/`done`, and a catch-all would let a
/// third spelling silently decode instead of surfacing as corruption.
///
/// [`FailureClass`]: crate::domain::backend_outcome::FailureClass
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    #[default]
    Open,
    Done,
}

impl Lifecycle {
    /// Storage spelling. ADR-0043 narrows `items.status` to these two values.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_open() {
        // Locally created work starts open. If this drifted to `Done`, every
        // `tk add` Item would be born terminal — ADR-0006 refuses to
        // transition out of `done`, so nothing could reopen it.
        assert_eq!(Lifecycle::default(), Lifecycle::Open);
    }

    #[test]
    fn text_matches_the_storage_spellings() {
        // These spellings and migration 011's `items.status` CHECK must
        // change together in one commit, or every row fails to decode.
        assert_eq!(Lifecycle::Open.text(), "open");
        assert_eq!(Lifecycle::Done.text(), "done");
    }

    #[test]
    fn every_variant_round_trips_through_its_storage_spelling() {
        // The payload-side counterpart to `text_matches_the_storage_spellings`:
        // `LifecycleChange` carries `Lifecycle` straight into `payload_json`, so a
        // drift between the serde spelling and `text()` would decode one Backend
        // Adapter's write as a different Lifecycle than it wrote.
        for (variant, text) in [(Lifecycle::Open, "open"), (Lifecycle::Done, "done")] {
            assert_eq!(variant.text(), text);
            assert_eq!(
                serde_json::to_string(&variant).unwrap(),
                format!(r#""{text}""#)
            );
            assert_eq!(
                serde_json::from_str::<Lifecycle>(&format!(r#""{text}""#)).unwrap(),
                variant
            );
        }
    }
}
