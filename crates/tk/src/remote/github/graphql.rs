//! Typed GraphQL operations owned by the GitHub Backend Adapter.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

#[cfg(test)]
pub(super) use self::transport::GraphqlCompletion;
pub(super) use self::transport::{
    GraphqlCompleted, GraphqlExchange, GraphqlRequest, GraphqlStartFailure, GraphqlTransport,
};

pub(super) mod cli;
mod transport;

/// Reads every enabled Issue Type page needed by ADR-0054 capability checks.
pub(super) const ISSUE_TYPES_QUERY: &str = "query RepositoryIssueTypes($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { issueTypes(first: 50, after: $after) { nodes { id name isEnabled } pageInfo { hasNextPage endCursor } } } }";
/// Reads the single-select issue fields an account defines (ADR-0054). The
/// account, not the repository, owns them; `hasNextPage` lets the caller refuse
/// to read a truncated list as "absent".
const ISSUE_FIELDS_SELECTION: &str = "issueFields(first: 50) { nodes { ... on IssueFieldSingleSelect { id name options { id name } } } pageInfo { hasNextPage } }";
/// Sets one single-select field value. Probed as an upsert (ADR-0054): an
/// unset field takes the value and a set one is replaced.
pub(super) const SET_ISSUE_FIELD_VALUE_QUERY: &str = "mutation SetIssueFieldValue($issueId: ID!, $issueFields: [IssueFieldCreateOrUpdateInput!]!) { setIssueFieldValue(input: { issueId: $issueId, issueFields: $issueFields }) { issue { number } } }";
/// Creates an issue with its optional native Issue Type and issue fields in
/// one effect (ADR-0054).
pub(super) const CREATE_ISSUE_QUERY: &str = "mutation CreateIssue($repositoryId: ID!, $title: String!, $body: String!, $issueTypeId: ID, $issueFields: [IssueFieldCreateOrUpdateInput!]) { createIssue(input: { repositoryId: $repositoryId, title: $title, body: $body, issueTypeId: $issueTypeId, issueFields: $issueFields }) { issue { url number } } }";

/// 50 Pull targets request at most 50 repositories, 2,500 issue fields, and
/// 2,500 issue field values, well below GitHub's 500,000-node query limit.
pub(super) const MAX_PULL_KEYS_PER_QUERY: usize = 50;

/// A typed GraphQL operation binds one request to its response decoder.
pub(super) trait GraphqlOperation {
    /// Response data accepted for this operation's GraphQL envelope.
    type Response: DeserializeOwned;

    /// Build the operation's host-bound document and variables.
    fn request(&self) -> GraphqlRequest;
}

/// Parsed operation evidence above the replaceable transport port.
pub(super) enum GraphqlObservation<T> {
    NotStarted(GraphqlStartFailure),
    Completed {
        exchange: GraphqlCompleted,
        envelope: Result<GraphqlEnvelope<T>, String>,
    },
    OutcomeUnobserved(String),
}

/// Execute one operation without collapsing transport and GraphQL evidence.
pub(super) fn execute<O: GraphqlOperation>(
    transport: &dyn GraphqlTransport,
    operation: &O,
) -> GraphqlObservation<O::Response> {
    match transport.exchange(&operation.request()) {
        GraphqlExchange::NotStarted(failure) => GraphqlObservation::NotStarted(failure),
        GraphqlExchange::OutcomeUnobserved(failure) => {
            GraphqlObservation::OutcomeUnobserved(failure)
        }
        GraphqlExchange::Completed(exchange) => {
            let envelope =
                serde_json::from_slice(&exchange.body).map_err(|error| error.to_string());
            GraphqlObservation::Completed { exchange, envelope }
        }
    }
}

/// Exhaustive Issue Type page read for one repository (ADR-0054).
pub(super) struct IssueTypesOperation<'a> {
    host: &'a str,
    owner: &'a str,
    name: &'a str,
    after: Option<&'a str>,
}

impl<'a> IssueTypesOperation<'a> {
    /// Bind one repository and optional continuation cursor to the page query.
    pub(super) fn new(
        host: &'a str,
        owner: &'a str,
        name: &'a str,
        after: Option<&'a str>,
    ) -> Self {
        Self {
            host,
            owner,
            name,
            after,
        }
    }
}

impl GraphqlOperation for IssueTypesOperation<'_> {
    type Response = IssueTypePageResponse;

    fn request(&self) -> GraphqlRequest {
        GraphqlRequest {
            host: self.host.into(),
            operation_name: "RepositoryIssueTypes",
            document: ISSUE_TYPES_QUERY.into(),
            variables: json!({
                "owner": self.owner,
                "name": self.name,
                "after": self.after,
            }),
        }
    }
}

/// A resolved single-select issue field value, ready for the wire.
#[derive(Debug, Clone, Copy)]
pub(super) struct FieldValue<'a> {
    pub field_id: &'a str,
    pub option_id: &'a str,
}

impl FieldValue<'_> {
    fn input(self) -> serde_json::Value {
        json!({"fieldId": self.field_id, "singleSelectOptionId": self.option_id})
    }
}

/// Read of an account's issue fields through one repository.
pub(super) struct IssueFieldsOperation<'a> {
    host: &'a str,
    owner: &'a str,
    name: &'a str,
}

impl<'a> IssueFieldsOperation<'a> {
    /// Bind one repository to the issue field query.
    pub(super) fn new(host: &'a str, owner: &'a str, name: &'a str) -> Self {
        Self { host, owner, name }
    }
}

impl GraphqlOperation for IssueFieldsOperation<'_> {
    type Response = IssueFieldsResponse;

    fn request(&self) -> GraphqlRequest {
        GraphqlRequest {
            host: self.host.into(),
            operation_name: "RepositoryIssueFields",
            document: format!(
                "query RepositoryIssueFields($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {ISSUE_FIELDS_SELECTION} }} }}"
            ),
            variables: json!({"owner": self.owner, "name": self.name}),
        }
    }
}

/// Replaces one issue field value on an existing issue (ADR-0054).
pub(super) struct SetIssueFieldValueOperation<'a> {
    host: &'a str,
    issue_id: &'a str,
    value: FieldValue<'a>,
}

impl<'a> SetIssueFieldValueOperation<'a> {
    /// Bind one issue node and its resolved field value to the mutation.
    pub(super) fn new(host: &'a str, issue_id: &'a str, value: FieldValue<'a>) -> Self {
        Self {
            host,
            issue_id,
            value,
        }
    }
}

impl GraphqlOperation for SetIssueFieldValueOperation<'_> {
    type Response = SetIssueFieldValueData;

    fn request(&self) -> GraphqlRequest {
        GraphqlRequest {
            host: self.host.into(),
            operation_name: "SetIssueFieldValue",
            document: SET_ISSUE_FIELD_VALUE_QUERY.into(),
            variables: json!({
                "issueId": self.issue_id,
                "issueFields": [self.value.input()],
            }),
        }
    }
}

/// Atomic GitHub issue creation with its resolved native Issue Type and issue
/// field values (ADR-0054). Either may be absent: a Task is untyped, and an
/// Epic has no Priority to set.
pub(super) struct CreateIssueOperation<'a> {
    host: &'a str,
    repository_id: &'a str,
    title: &'a str,
    body: &'a str,
    issue_type_id: Option<&'a str>,
    priority: Option<FieldValue<'a>>,
}

impl<'a> CreateIssueOperation<'a> {
    /// Bind the retained Promotion snapshot and its resolved taxonomy to
    /// creation.
    pub(super) fn new(
        host: &'a str,
        repository_id: &'a str,
        title: &'a str,
        body: &'a str,
        issue_type_id: Option<&'a str>,
        priority: Option<FieldValue<'a>>,
    ) -> Self {
        Self {
            host,
            repository_id,
            title,
            body,
            issue_type_id,
            priority,
        }
    }
}

impl GraphqlOperation for CreateIssueOperation<'_> {
    type Response = CreateIssueData;

    fn request(&self) -> GraphqlRequest {
        GraphqlRequest {
            host: self.host.into(),
            operation_name: "CreateIssue",
            document: CREATE_ISSUE_QUERY.into(),
            variables: json!({
                "repositoryId": self.repository_id,
                "title": self.title,
                "body": self.body,
                "issueTypeId": self.issue_type_id,
                "issueFields": self.priority.map(|value| [value.input()]),
            }),
        }
    }
}

/// One canonical GitHub Issue address inside a bounded Pull operation.
#[derive(Debug)]
pub(super) struct PullTarget {
    pub input_index: usize,
    pub owner: String,
    pub name: String,
    pub number: i64,
}

impl PullTarget {
    /// Deterministic top-level response alias for this input position.
    #[must_use]
    pub(super) fn alias(&self) -> String {
        format!("item_{}", self.input_index)
    }
}

/// Exact-key Pull query for one host and one bounded target chunk.
pub(super) struct PullOperation<'a> {
    host: &'a str,
    targets: &'a [PullTarget],
}

impl<'a> PullOperation<'a> {
    /// Bind one host-local chunk to its generated query and variables.
    pub(super) fn new(host: &'a str, targets: &'a [PullTarget]) -> Self {
        Self { host, targets }
    }
}

impl GraphqlOperation for PullOperation<'_> {
    type Response = PullData;

    fn request(&self) -> GraphqlRequest {
        let definitions = self
            .targets
            .iter()
            .flat_map(|target| {
                let index = target.input_index;
                [
                    format!("$owner_{index}: String!"),
                    format!("$name_{index}: String!"),
                    format!("$number_{index}: Int!"),
                ]
            })
            .collect::<Vec<_>>()
            .join(", ");
        let selections = self
            .targets
            .iter()
            .map(|target| {
                let index = target.input_index;
                let alias = target.alias();
                format!(
                    "{alias}: repository(owner: $owner_{index}, name: $name_{index}) {{ {ISSUE_FIELDS_SELECTION} item: issueOrPullRequest(number: $number_{index}) {{ __typename ... on Issue {{ number title body state url issueType {{ name }} issueFieldValues(first: 50) {{ nodes {{ ... on IssueFieldSingleSelectValue {{ value field {{ ... on IssueFieldSingleSelect {{ name }} }} }} }} pageInfo {{ hasNextPage }} }} }} ... on PullRequest {{ number url }} }} }}"
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut variables = serde_json::Map::new();
        for target in self.targets {
            let index = target.input_index;
            variables.insert(format!("owner_{index}"), json!(target.owner));
            variables.insert(format!("name_{index}"), json!(target.name));
            variables.insert(format!("number_{index}"), json!(target.number));
        }
        GraphqlRequest {
            host: self.host.into(),
            operation_name: "PullItems",
            document: format!("query PullItems({definitions}) {{ {selections} }}"),
            variables: variables.into(),
        }
    }
}

/// Standard GraphQL response envelope before operation-specific policy.
#[derive(Debug, Deserialize)]
pub(super) struct GraphqlEnvelope<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<GraphqlError>,
}

/// One GraphQL error with the optional response path used by Pull aliases.
#[derive(Debug, Deserialize)]
pub(super) struct GraphqlError {
    pub message: String,
    pub path: Option<Vec<serde_json::Value>>,
}

impl GraphqlError {
    /// Consume the wire error and retain its user-facing message.
    pub(super) fn into_message(self) -> String {
        self.message
    }

    /// Top-level response field named by the first path segment, when known.
    pub(super) fn root_field(&self) -> Option<&str> {
        self.path.as_deref()?.first()?.as_str()
    }
}

/// `createIssue` mutation response data (ADR-0054).
#[derive(Debug, Deserialize)]
pub(super) struct CreateIssueData {
    #[serde(rename = "createIssue")]
    pub create_issue: Option<CreateIssuePayload>,
}

/// `createIssue` payload containing the optional creation receipt.
#[derive(Debug, Deserialize)]
pub(super) struct CreateIssuePayload {
    pub issue: Option<CreateIssueReceipt>,
}

/// Canonical URL receipt that proves GitHub Issue creation.
#[derive(Debug, Deserialize)]
pub(super) struct CreateIssueReceipt {
    pub url: String,
}

/// `setIssueFieldValue` mutation response data (ADR-0054).
#[derive(Debug, Deserialize)]
pub(super) struct SetIssueFieldValueData {
    #[serde(rename = "setIssueFieldValue")]
    pub set_issue_field_value: Option<SetIssueFieldValuePayload>,
}

/// `setIssueFieldValue` payload; the issue proves the write landed.
#[derive(Debug, Deserialize)]
pub(super) struct SetIssueFieldValuePayload {
    pub issue: Option<serde_json::Value>,
}

/// Repository data returned by the issue field query.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldsResponse {
    pub repository: Option<IssueFieldsRepository>,
}

/// Repository wrapper for GitHub's nullable issue field connection.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldsRepository {
    #[serde(rename = "issueFields")]
    pub issue_fields: Option<IssueFieldConnection>,
}

/// One page of an account's issue fields. A field that is not a single-select
/// decodes with every member absent.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldConnection {
    pub nodes: Vec<IssueFieldNode>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

/// A single-select issue field and its options.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldNode {
    pub id: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub options: Vec<IssueFieldOption>,
}

/// One option of a single-select issue field.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldOption {
    pub id: String,
    pub name: String,
}

/// Issue field values on one issue, as Pull reads them.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldValues {
    pub nodes: Vec<IssueFieldValueNode>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

/// One issue field value. Non-single-select values decode with both members
/// absent.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldValueNode {
    pub value: Option<String>,
    pub field: Option<IssueFieldName>,
}

/// The owning field's name, the only part of it Pull reads.
#[derive(Debug, Deserialize)]
pub(super) struct IssueFieldName {
    pub name: Option<String>,
}

/// Repository data returned by one Issue Type page query.
#[derive(Debug, Deserialize)]
pub(super) struct IssueTypePageResponse {
    pub repository: Option<IssueTypeRepository>,
}

/// Repository wrapper for GitHub's nullable Issue Type connection.
#[derive(Debug, Deserialize)]
pub(super) struct IssueTypeRepository {
    #[serde(rename = "issueTypes")]
    pub issue_types: IssueTypesField,
}

/// Keeps the initial-null policy distinct from a missing field.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum IssueTypesField {
    Connection(IssueTypeConnection),
    Null(()),
}

/// One page of native Issue Types and its continuation evidence.
#[derive(Debug, Deserialize)]
pub(super) struct IssueTypeConnection {
    pub nodes: Vec<NativeIssueType>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

/// Native Issue Type fields needed to find an enabled type by name.
#[derive(Debug, Deserialize)]
pub(super) struct NativeIssueType {
    pub id: String,
    pub name: String,
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
}

/// GraphQL connection state used for exhaustive pagination.
#[derive(Debug, Deserialize)]
pub(super) struct PageInfo {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub end_cursor: Option<String>,
}

/// Decoded top-level aliases from an exact-key Pull query.
#[derive(Debug, Deserialize)]
pub(super) struct PullData {
    #[serde(flatten)]
    pub items: std::collections::HashMap<String, Option<PullRepository>>,
}

/// Requested object returned for one alias, with the account's issue fields
/// that give its Priority meaning.
#[derive(Debug, Deserialize)]
pub(super) struct PullRepository {
    #[serde(rename = "issueFields")]
    pub issue_fields: Option<IssueFieldConnection>,
    pub item: Option<PullObject>,
}

/// GitHub's issue-or-pull-request union before Backend Item mapping.
#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
pub(super) enum PullObject {
    Issue {
        #[serde(flatten)]
        issue: super::GhIssue,
    },
    PullRequest {
        number: i64,
        url: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeTransport;

    impl GraphqlTransport for FakeTransport {
        fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
            assert_eq!(request.host, "github.example.com");
            assert_eq!(request.operation_name, "RepositoryIssueTypes");
            assert_eq!(request.document, ISSUE_TYPES_QUERY);
            assert_eq!(
                request.variables,
                json!({"owner": "o", "name": "r", "after": null})
            );
            GraphqlExchange::Completed(GraphqlCompleted {
                body: br#"{"data":{"repository":null},"errors":[{"message":"denied","path":["repository",0]}]}"#.to_vec(),
                completion: GraphqlCompletion::Failed {
                    detail: "GraphQL: denied".into(),
                },
            })
        }
    }

    #[test]
    fn typed_operation_keeps_data_errors_paths_and_transport_diagnostics() {
        let operation = IssueTypesOperation::new("github.example.com", "o", "r", None);

        let GraphqlObservation::Completed { exchange, envelope } =
            execute(&FakeTransport, &operation)
        else {
            panic!("fake returns a completed exchange");
        };
        assert!(matches!(
            exchange.completion,
            GraphqlCompletion::Failed { ref detail } if detail == "GraphQL: denied"
        ));
        let envelope = envelope.unwrap();
        assert!(matches!(
            envelope.data,
            Some(IssueTypePageResponse { repository: None })
        ));
        let path = envelope.errors.into_iter().next().unwrap().path.unwrap();
        assert_eq!(path, [json!("repository"), json!(0)]);
    }

    #[test]
    fn pull_operation_builds_each_selection_and_variable_independently() {
        let targets = [
            PullTarget {
                input_index: 2,
                owner: "one".into(),
                name: "alpha".into(),
                number: 7,
            },
            PullTarget {
                input_index: 5,
                owner: "two".into(),
                name: "beta".into(),
                number: 11,
            },
        ];

        let request = PullOperation::new("github.example.com", &targets).request();

        assert_eq!(request.host, "github.example.com");
        assert_eq!(request.operation_name, "PullItems");
        assert_eq!(
            request.variables,
            json!({
                "owner_2": "one",
                "name_2": "alpha",
                "number_2": 7,
                "owner_5": "two",
                "name_5": "beta",
                "number_5": 11,
            })
        );
        assert_eq!(
            request.document,
            "query PullItems($owner_2: String!, $name_2: String!, $number_2: Int!, \
$owner_5: String!, $name_5: String!, $number_5: Int!) { item_2: \
repository(owner: $owner_2, name: $name_2) { issueFields(first: 50) { nodes { \
... on IssueFieldSingleSelect { id name options { id name } } } pageInfo { \
hasNextPage } } item: issueOrPullRequest(number: $number_2) { __typename ... on \
Issue { number title body state url issueType { name } issueFieldValues(first: \
50) { nodes { ... on IssueFieldSingleSelectValue { value field { ... on \
IssueFieldSingleSelect { name } } } } pageInfo { hasNextPage } } } ... on \
PullRequest { number url } } } item_5: repository(owner: $owner_5, name: \
$name_5) { issueFields(first: 50) { nodes { ... on IssueFieldSingleSelect { id \
name options { id name } } } pageInfo { hasNextPage } } item: \
issueOrPullRequest(number: $number_5) { __typename ... on Issue { number title \
body state url issueType { name } issueFieldValues(first: 50) { nodes { ... on \
IssueFieldSingleSelectValue { value field { ... on IssueFieldSingleSelect { \
name } } } } pageInfo { hasNextPage } } } ... on PullRequest { number url } } } }"
        );
    }
}
