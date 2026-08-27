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

/// Reads every enabled Issue Type page needed by ADR-0021 capability checks.
pub(super) const ISSUE_TYPES_QUERY: &str = "query RepositoryIssueTypes($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { issueTypes(first: 50, after: $after) { nodes { id name isEnabled } pageInfo { hasNextPage endCursor } } } }";
/// Reads candidate Bug Labels and pagination evidence for ADR-0021 fallback.
pub(super) const LABELS_QUERY: &str = "query RepositoryLabels($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { labels(first: 100, query: \"bug\", after: $after) { nodes { id name } pageInfo { hasNextPage endCursor } } } }";
/// Creates a Bug and its one chosen representation in one effect (ADR-0021).
pub(super) const CREATE_BUG_QUERY: &str = "mutation CreateBugIssue($repositoryId: ID!, $title: String!, $body: String!, $issueTypeId: ID, $labelIds: [ID!]) { createIssue(input: { repositoryId: $repositoryId, title: $title, body: $body, issueTypeId: $issueTypeId, labelIds: $labelIds }) { issue { url number } } }";

/// 50 Pull targets request at most 5,100 nodes: one Repository, one Item, and
/// up to 100 Labels per target, well below GitHub's 500,000-node query limit.
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

/// Exhaustive Issue Type page read for one repository (ADR-0021).
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

/// Candidate Bug Label page read for one personal repository (ADR-0021).
pub(super) struct LabelsOperation<'a> {
    host: &'a str,
    owner: &'a str,
    name: &'a str,
    after: Option<&'a str>,
}

impl<'a> LabelsOperation<'a> {
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

impl GraphqlOperation for LabelsOperation<'_> {
    type Response = LabelPageResponse;

    fn request(&self) -> GraphqlRequest {
        GraphqlRequest {
            host: self.host.into(),
            operation_name: "RepositoryLabels",
            document: LABELS_QUERY.into(),
            variables: json!({
                "owner": self.owner,
                "name": self.name,
                "after": self.after,
            }),
        }
    }
}

/// Atomic GitHub Bug creation with one resolved representation (ADR-0021).
pub(super) struct CreateBugOperation<'a> {
    host: &'a str,
    repository_id: &'a str,
    title: &'a str,
    body: &'a str,
    issue_type_id: Option<&'a str>,
    label_id: Option<&'a str>,
}

impl<'a> CreateBugOperation<'a> {
    /// Bind the retained Promotion snapshot and one representation to creation.
    pub(super) fn new(
        host: &'a str,
        repository_id: &'a str,
        title: &'a str,
        body: &'a str,
        issue_type_id: Option<&'a str>,
        label_id: Option<&'a str>,
    ) -> Self {
        Self {
            host,
            repository_id,
            title,
            body,
            issue_type_id,
            label_id,
        }
    }
}

impl GraphqlOperation for CreateBugOperation<'_> {
    type Response = CreateIssueData;

    fn request(&self) -> GraphqlRequest {
        let label_ids = self.label_id.map(|id| vec![id]);
        GraphqlRequest {
            host: self.host.into(),
            operation_name: "CreateBugIssue",
            document: CREATE_BUG_QUERY.into(),
            variables: json!({
                "repositoryId": self.repository_id,
                "title": self.title,
                "body": self.body,
                "issueTypeId": self.issue_type_id,
                "labelIds": label_ids,
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
                    "{alias}: repository(owner: $owner_{index}, name: $name_{index}) {{ isInOrganization item: issueOrPullRequest(number: $number_{index}) {{ __typename ... on Issue {{ number title body state url issueType {{ name }} labels(first: 100) {{ nodes {{ name }} }} }} ... on PullRequest {{ number url }} }} }}"
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

/// `createIssue` mutation response data (ADR-0021).
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

/// Keeps ADR-0021's initial-null policy distinct from a missing field.
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

/// Native Issue Type fields needed to find an enabled Bug type.
#[derive(Debug, Deserialize)]
pub(super) struct NativeIssueType {
    pub id: String,
    pub name: String,
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
}

/// Repository data returned by one Label page query.
#[derive(Debug, Deserialize)]
pub(super) struct LabelPageResponse {
    pub repository: Option<LabelRepository>,
}

/// Repository wrapper for the Label connection.
#[derive(Debug, Deserialize)]
pub(super) struct LabelRepository {
    pub labels: LabelConnection,
}

/// One Label page and its continuation evidence.
#[derive(Debug, Deserialize)]
pub(super) struct LabelConnection {
    pub nodes: Vec<GraphqlLabel>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

/// GitHub Label fields needed for exact Bug fallback matching.
#[derive(Debug, Deserialize)]
pub(super) struct GraphqlLabel {
    pub id: String,
    pub name: String,
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

/// Repository ownership and requested object returned for one alias.
#[derive(Debug, Deserialize)]
pub(super) struct PullRepository {
    #[serde(rename = "isInOrganization")]
    pub is_in_organization: bool,
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
repository(owner: $owner_2, name: $name_2) { isInOrganization item: \
issueOrPullRequest(number: $number_2) { __typename ... on Issue { number title \
body state url issueType { name } labels(first: 100) { nodes { name } } } ... \
on PullRequest { number url } } } item_5: repository(owner: $owner_5, name: \
$name_5) { isInOrganization item: issueOrPullRequest(number: $number_5) { \
__typename ... on Issue { number title body state url issueType { name } \
labels(first: 100) { nodes { name } } } ... on PullRequest { number url } } } }"
        );
    }
}
