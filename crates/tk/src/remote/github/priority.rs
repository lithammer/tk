//! Priority as GitHub's native single-select issue field (ADR-0054).

use crate::domain::priority::Priority;
use crate::remote::adapter::AdapterReadError;

use super::graphql::{FieldValue, IssueFieldConnection, IssueFieldValues};

/// The account's field that carries Priority, matched case-insensitively.
const FIELD_NAME: &str = "Priority";

/// Each tk level and the option GitHub's starter pack names for it.
const OPTIONS: [(Priority, &str); 4] = [
    (Priority::P0, "Urgent"),
    (Priority::P1, "High"),
    (Priority::P2, "Medium"),
    (Priority::P3, "Low"),
];

/// The Priority field and the option ID of every level, resolved for one
/// account. Holding all four proves the account can round-trip each level.
#[derive(Debug)]
pub(super) struct PriorityField {
    id: String,
    options: Vec<(Priority, String)>,
}

impl PriorityField {
    /// Find the Priority field in `fields`, or fail naming it and `owner`.
    ///
    /// A missing field is an error and never "no value": a deleted field reads
    /// back as unset on every issue, which would overwrite every local
    /// Priority on the next Pull.
    pub(super) fn resolve(
        fields: Option<IssueFieldConnection>,
        owner: &str,
    ) -> Result<Self, AdapterReadError> {
        let missing = || {
            AdapterReadError::Failed(format!(
                "required issue field '{FIELD_NAME}' is missing on {owner}"
            ))
        };
        let fields = fields.ok_or_else(missing)?;
        let Some(field) = fields.nodes.into_iter().find(|field| {
            field
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(FIELD_NAME))
        }) else {
            return Err(if fields.page_info.has_next_page {
                AdapterReadError::Failed(format!(
                    "GitHub listed only part of the issue fields on {owner}, \
                     so '{FIELD_NAME}' could not be found"
                ))
            } else {
                missing()
            });
        };
        let id = field.id.ok_or_else(missing)?;
        let options = OPTIONS
            .iter()
            .map(|(priority, name)| {
                field
                    .options
                    .iter()
                    .find(|option| option.name.eq_ignore_ascii_case(name))
                    .map(|option| (*priority, option.id.clone()))
                    .ok_or_else(|| {
                        AdapterReadError::Failed(format!(
                            "required issue field '{FIELD_NAME}' lacks option '{name}' on {owner}"
                        ))
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { id, options })
    }

    /// The wire value that sets `priority`.
    pub(super) fn value(&self, priority: Priority) -> FieldValue<'_> {
        let (_, option_id) = self
            .options
            .iter()
            .find(|(level, _)| *level == priority)
            .expect("every level resolved");
        FieldValue {
            field_id: &self.id,
            option_id,
        }
    }
}

/// The Priority an issue's field values hold. An issue with no Priority, or
/// with an option tk does not name, is the default `P2` (ADR-0054). A list
/// GitHub truncated that holds no Priority is an error, never "no value".
pub(super) fn from_values(
    values: Option<&IssueFieldValues>,
    owner: &str,
) -> Result<Priority, AdapterReadError> {
    let found = values
        .into_iter()
        .flat_map(|values| &values.nodes)
        .find(|node| {
            node.field
                .as_ref()
                .and_then(|field| field.name.as_deref())
                .is_some_and(|name| name.eq_ignore_ascii_case(FIELD_NAME))
        });
    if found.is_none() && values.is_some_and(|values| values.page_info.has_next_page) {
        return Err(AdapterReadError::Failed(format!(
            "GitHub listed only part of an issue's field values on {owner}, \
             so '{FIELD_NAME}' could not be found"
        )));
    }
    Ok(found
        .and_then(|node| node.value.as_deref())
        .and_then(|option| {
            OPTIONS
                .iter()
                .find(|(_, name)| name.eq_ignore_ascii_case(option))
        })
        .map_or_else(Priority::default, |(priority, _)| *priority))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(json: &str) -> IssueFieldValues {
        serde_json::from_str(json).unwrap()
    }

    fn priority_value(option: &str) -> IssueFieldValues {
        values(&format!(
            r#"{{"nodes":[{{"value":"Low","field":{{"name":"Effort"}}}},
                {{"value":"{option}","field":{{"name":"Priority"}}}}],
                "pageInfo":{{"hasNextPage":false}}}}"#
        ))
    }

    #[test]
    fn each_github_option_reads_as_its_own_level() {
        // Distinct per level: a swapped or misspelled row in `OPTIONS` would
        // change a pushed Priority when it came back on the next Pull.
        for (option, expected) in [
            ("Urgent", Priority::P0),
            ("High", Priority::P1),
            ("Medium", Priority::P2),
            ("Low", Priority::P3),
            ("urgent", Priority::P0),
        ] {
            assert_eq!(
                from_values(Some(&priority_value(option)), "o").unwrap(),
                expected,
                "{option}"
            );
        }
    }

    #[test]
    fn no_value_or_an_unnamed_option_reads_as_the_default() {
        let other_field = values(
            r#"{"nodes":[{"value":"Urgent","field":{"name":"Effort"}}],"pageInfo":{"hasNextPage":false}}"#,
        );
        let unnamed = priority_value("Someday");
        let empty = values(r#"{"nodes":[],"pageInfo":{"hasNextPage":false}}"#);
        for values in [None, Some(&other_field), Some(&unnamed), Some(&empty)] {
            assert_eq!(from_values(values, "o").unwrap(), Priority::P2);
        }
    }

    #[test]
    fn a_truncated_list_without_priority_is_an_error_not_no_value() {
        let truncated = values(
            r#"{"nodes":[{"value":"Low","field":{"name":"Effort"}}],"pageInfo":{"hasNextPage":true}}"#,
        );
        assert!(from_values(Some(&truncated), "o").is_err());
    }
}
