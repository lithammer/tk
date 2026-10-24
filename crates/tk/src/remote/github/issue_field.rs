//! Priority and Effort as GitHub's native single-select issue fields
//! (ADR-0054).

use crate::domain::backend_operation::SharedTicketFields;
use crate::domain::effort::Effort;
use crate::domain::priority::Priority;
use crate::remote::adapter::AdapterReadError;

use super::graphql::{FieldValue, IssueFieldConnection, IssueFieldValues};

/// A tk value that crosses as one single-select issue field.
pub(super) trait SingleSelect: Copy + Eq + 'static {
    /// The account's field, matched case-insensitively.
    const FIELD_NAME: &'static str;
    /// Each tk value and the option GitHub's starter pack names for it.
    const OPTIONS: &'static [(Self, &'static str)];
}

impl SingleSelect for Priority {
    const FIELD_NAME: &'static str = "Priority";
    const OPTIONS: &'static [(Self, &'static str)] = &[
        (Priority::P0, "Urgent"),
        (Priority::P1, "High"),
        (Priority::P2, "Medium"),
        (Priority::P3, "Low"),
    ];
}

impl SingleSelect for Effort {
    const FIELD_NAME: &'static str = "Effort";
    const OPTIONS: &'static [(Self, &'static str)] = &[
        (Effort::Low, "Low"),
        (Effort::Medium, "Medium"),
        (Effort::High, "High"),
    ];
}

/// One field and the option ID of every value, resolved for one account.
/// Holding every option proves the account can round-trip each value.
#[derive(Debug)]
pub(super) struct ResolvedField<T> {
    id: String,
    options: Vec<(T, String)>,
}

impl<T: SingleSelect> ResolvedField<T> {
    /// Find the field in `fields`, or fail naming it and `owner`.
    ///
    /// A missing field is an error and never "no value": a deleted field reads
    /// back as unset on every issue, which would overwrite every local value on
    /// the next Pull.
    pub(super) fn resolve(
        fields: Option<&IssueFieldConnection>,
        owner: &str,
    ) -> Result<Self, AdapterReadError> {
        let field_name = T::FIELD_NAME;
        let missing = || {
            AdapterReadError::Failed(format!(
                "required issue field '{field_name}' is missing on {owner}"
            ))
        };
        let fields = fields.ok_or_else(missing)?;
        let Some(field) = fields.nodes.iter().find(|field| {
            field
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(field_name))
        }) else {
            return Err(if fields.page_info.has_next_page {
                AdapterReadError::Failed(format!(
                    "GitHub listed only part of the issue fields on {owner}, \
                     so '{field_name}' could not be found"
                ))
            } else {
                missing()
            });
        };
        let id = field.id.clone().ok_or_else(missing)?;
        let options = T::OPTIONS
            .iter()
            .map(|(value, name)| {
                field
                    .options
                    .iter()
                    .find(|option| option.name.eq_ignore_ascii_case(name))
                    .map(|option| (*value, option.id.clone()))
                    .ok_or_else(|| {
                        AdapterReadError::Failed(format!(
                            "required issue field '{field_name}' lacks option '{name}' on {owner}"
                        ))
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { id, options })
    }

    pub(super) fn id(&self) -> &str {
        &self.id
    }

    /// The wire value that sets `value`.
    pub(super) fn value(&self, value: T) -> FieldValue<'_> {
        let (_, option_id) = self
            .options
            .iter()
            .find(|(option, _)| *option == value)
            .expect("every option resolved");
        FieldValue {
            field_id: &self.id,
            option_id,
        }
    }
}

/// The Priority and Effort fields a Ticket issue needs, resolved together:
/// Ticket Promotion and Pull require both (ADR-0054).
#[derive(Debug)]
pub(super) struct ResolvedFields {
    priority: ResolvedField<Priority>,
    effort: ResolvedField<Effort>,
}

impl ResolvedFields {
    pub(super) fn resolve(
        fields: Option<&IssueFieldConnection>,
        owner: &str,
    ) -> Result<Self, AdapterReadError> {
        Ok(Self {
            priority: ResolvedField::resolve(fields, owner)?,
            effort: ResolvedField::resolve(fields, owner)?,
        })
    }

    /// The wire values that set `fields`; an unset Effort is left out.
    pub(super) fn values(&self, fields: SharedTicketFields) -> Vec<FieldValue<'_>> {
        std::iter::once(self.priority.value(fields.priority))
            .chain(fields.effort.map(|effort| self.effort.value(effort)))
            .collect()
    }
}

/// The Priority and Effort an issue's field values hold. No Priority, or an
/// option tk does not name, reads as the default `P2`; no Effort reads as none
/// (ADR-0054).
pub(super) fn read_ticket_fields(
    values: Option<&IssueFieldValues>,
    owner: &str,
) -> Result<SharedTicketFields, AdapterReadError> {
    Ok(SharedTicketFields {
        priority: read::<Priority>(values, owner)?.unwrap_or_default(),
        effort: read::<Effort>(values, owner)?,
    })
}

/// The value an issue holds for `T`, or `None` when it holds none or an option
/// tk does not name. A list GitHub truncated that lacks the field is an error,
/// never "no value".
fn read<T: SingleSelect>(
    values: Option<&IssueFieldValues>,
    owner: &str,
) -> Result<Option<T>, AdapterReadError> {
    let field_name = T::FIELD_NAME;
    let found = values
        .into_iter()
        .flat_map(|values| &values.nodes)
        .find(|node| {
            node.field
                .as_ref()
                .and_then(|field| field.name.as_deref())
                .is_some_and(|name| name.eq_ignore_ascii_case(field_name))
        });
    if found.is_none() && values.is_some_and(|values| values.page_info.has_next_page) {
        return Err(AdapterReadError::Failed(format!(
            "GitHub listed only part of an issue's field values on {owner}, \
             so '{field_name}' could not be found"
        )));
    }
    Ok(found
        .and_then(|node| node.value.as_deref())
        .and_then(|option| {
            T::OPTIONS
                .iter()
                .find(|(_, name)| name.eq_ignore_ascii_case(option))
        })
        .map(|(value, _)| *value))
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
                read_ticket_fields(Some(&priority_value(option)), "o")
                    .unwrap()
                    .priority,
                expected,
                "{option}"
            );
        }
    }

    #[test]
    fn each_github_option_reads_as_its_own_effort() {
        // Effort's options share names with Priority's but not their order: a
        // row copied from Priority's table would swap Low and High, and every
        // pushed Effort would come back flipped on the next Pull.
        for (option, expected) in [
            ("Low", Effort::Low),
            ("Medium", Effort::Medium),
            ("High", Effort::High),
            ("high", Effort::High),
        ] {
            let values = values(&format!(
                r#"{{"nodes":[{{"value":"Urgent","field":{{"name":"Priority"}}}},
                    {{"value":"{option}","field":{{"name":"Effort"}}}}],
                    "pageInfo":{{"hasNextPage":false}}}}"#
            ));
            assert_eq!(
                read_ticket_fields(Some(&values), "o").unwrap().effort,
                Some(expected),
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
            assert_eq!(
                read_ticket_fields(values, "o").unwrap().priority,
                Priority::P2
            );
        }
    }

    #[test]
    fn a_truncated_list_without_priority_is_an_error_not_no_value() {
        let truncated = values(
            r#"{"nodes":[{"value":"Low","field":{"name":"Effort"}}],"pageInfo":{"hasNextPage":true}}"#,
        );
        assert!(read_ticket_fields(Some(&truncated), "o").is_err());
    }
}
