//! GitHub response bodies shared by the Adapter, sync, and command tests.
//!
//! A test that needs a deliberately broken field list or issue object states
//! it inline.

use serde_json::{Value, json};

pub(crate) const PRIORITY_FIELD_ID: &str = "F_PRIORITY";

pub(crate) const EFFORT_FIELD_ID: &str = "F_EFFORT";

/// The ID of the Priority option named `Medium`.
pub(crate) const PRIORITY_MEDIUM_OPTION_ID: &str = "O_P2";

/// The account's Priority field, `F_PRIORITY` with the options `Urgent`..`Low`
/// as `O_P0`..`O_P3`, and its Effort field, `F_EFFORT` with `O_EL`..`O_EH`.
fn ticket_fields() -> Value {
    json!({
        "nodes": [{"id": PRIORITY_FIELD_ID, "name": "Priority", "options": [
            {"id": "O_P0", "name": "Urgent"},
            {"id": "O_P1", "name": "High"},
            {"id": PRIORITY_MEDIUM_OPTION_ID, "name": "Medium"},
            {"id": "O_P3", "name": "Low"},
        ]}, {"id": EFFORT_FIELD_ID, "name": "Effort", "options": [
            {"id": "O_EL", "name": "Low"},
            {"id": "O_EM", "name": "Medium"},
            {"id": "O_EH", "name": "High"},
        ]}],
        "pageInfo": {"hasNextPage": false},
    })
}

/// The repository-level issue field read that resolves [`ticket_fields`].
pub(crate) fn issue_fields_response() -> String {
    json!({"data": {"repository": {"issueFields": ticket_fields()}}}).to_string()
}

/// One Issue in a Pull response; `Default` is an open, untyped, unblocked
/// issue 1 in `github.com/o/r`.
#[derive(Clone, Copy)]
pub(crate) struct PullIssue<'a> {
    pub host: &'a str,
    pub owner: &'a str,
    pub name: &'a str,
    pub number: i64,
    pub title: &'a str,
    pub body: &'a str,
    pub state: &'a str,
    pub issue_type: Option<&'a str>,
    /// URLs of the issues blocking this one; `None` is a null node.
    pub blocked_by: &'a [Option<&'a str>],
    /// Whether GitHub cut the blocker list short.
    pub blocked_by_has_next_page: bool,
}

impl Default for PullIssue<'_> {
    fn default() -> Self {
        Self {
            host: "github.com",
            owner: "o",
            name: "r",
            number: 1,
            title: "",
            body: "",
            state: "OPEN",
            issue_type: None,
            blocked_by: &[],
            blocked_by_has_next_page: false,
        }
    }
}

/// The `item` object a Pull returns for `issue`.
pub(crate) fn pull_item(issue: PullIssue<'_>) -> Value {
    let blockers: Vec<Value> = issue
        .blocked_by
        .iter()
        .map(|url| url.map_or(Value::Null, |url| json!({"url": url})))
        .collect();
    json!({
        "__typename": "Issue",
        "number": issue.number,
        "title": issue.title,
        "body": issue.body,
        "state": issue.state,
        "url": format!(
            "https://{}/{}/{}/issues/{}",
            issue.host, issue.owner, issue.name, issue.number
        ),
        "issueType": issue.issue_type.map(|name| json!({"name": name})),
        "blockedBy": {
            "nodes": blockers,
            "pageInfo": {"hasNextPage": issue.blocked_by_has_next_page},
        },
    })
}

/// A Pull response body that answers each `(input index, item)` with the
/// account's fields and `item`.
pub(crate) fn pull_response(items: &[(usize, Value)]) -> Vec<u8> {
    let data: serde_json::Map<String, Value> = items
        .iter()
        .map(|(index, item)| {
            (
                format!("item_{index}"),
                json!({"issueFields": ticket_fields(), "item": item}),
            )
        })
        .collect();
    serde_json::to_vec(&json!({"data": data})).unwrap()
}
