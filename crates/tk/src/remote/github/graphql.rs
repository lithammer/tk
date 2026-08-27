//! Typed GraphQL operations owned by the GitHub Backend Adapter.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

pub(super) use self::transport::{GraphqlCompleted, GraphqlTransport};
use self::transport::{GraphqlExchange, GraphqlRequest, GraphqlTransportFailure};

pub(super) mod cli;
mod transport;

pub(super) const ISSUE_TYPES_QUERY: &str = "query RepositoryIssueTypes($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { issueTypes(first: 50, after: $after) { nodes { id name isEnabled } pageInfo { hasNextPage endCursor } } } }";
pub(super) const LABELS_QUERY: &str = "query RepositoryLabels($owner: String!, $name: String!, $after: String) { repository(owner: $owner, name: $name) { labels(first: 100, query: \"bug\", after: $after) { nodes { id name } pageInfo { hasNextPage endCursor } } } }";
pub(super) const CREATE_BUG_QUERY: &str = "mutation CreateBugIssue($repositoryId: ID!, $title: String!, $body: String!, $issueTypeId: ID, $labelIds: [ID!]) { createIssue(input: { repositoryId: $repositoryId, title: $title, body: $body, issueTypeId: $issueTypeId, labelIds: $labelIds }) { issue { url number } } }";

/// A typed GraphQL operation binds one request to its response decoder.
pub(super) trait GraphqlOperation {
    type Response: DeserializeOwned;

    fn request(&self) -> GraphqlRequest;
}

/// Parsed operation evidence above the replaceable transport port.
pub(super) enum GraphqlObservation<T> {
    NotStarted(GraphqlTransportFailure),
    Completed {
        exchange: GraphqlCompleted,
        envelope: Result<GraphqlEnvelope<T>, String>,
    },
    OutcomeUnobserved(GraphqlTransportFailure),
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

pub(super) struct IssueTypesOperation<'a> {
    host: &'a str,
    owner: &'a str,
    name: &'a str,
    after: Option<&'a str>,
}

impl<'a> IssueTypesOperation<'a> {
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

pub(super) struct LabelsOperation<'a> {
    host: &'a str,
    owner: &'a str,
    name: &'a str,
    after: Option<&'a str>,
}

impl<'a> LabelsOperation<'a> {
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

pub(super) struct CreateBugOperation<'a> {
    host: &'a str,
    repository_id: &'a str,
    title: &'a str,
    body: &'a str,
    issue_type_id: Option<&'a str>,
    label_id: Option<&'a str>,
}

impl<'a> CreateBugOperation<'a> {
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

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlEnvelope<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlError {
    pub message: String,
    pub path: Option<Vec<GraphqlPathSegment>>,
}

impl GraphqlError {
    pub(super) fn into_message(self) -> String {
        let _path = self.path.map(|path| {
            path.into_iter()
                .map(|segment| match segment {
                    GraphqlPathSegment::Field(field) => field,
                    GraphqlPathSegment::Index(index) => index.to_string(),
                })
                .collect::<Vec<_>>()
        });
        self.message
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum GraphqlPathSegment {
    Field(String),
    Index(usize),
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateIssueData {
    #[serde(rename = "createIssue")]
    pub create_issue: Option<CreateIssuePayload>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateIssuePayload {
    pub issue: Option<CreateIssueReceipt>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateIssueReceipt {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueTypePageResponse {
    pub repository: Option<IssueTypeRepository>,
}

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

#[derive(Debug, Deserialize)]
pub(super) struct IssueTypeConnection {
    pub nodes: Vec<NativeIssueType>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
pub(super) struct NativeIssueType {
    pub id: String,
    pub name: String,
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct LabelPageResponse {
    pub repository: Option<LabelRepository>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LabelRepository {
    pub labels: LabelConnection,
}

#[derive(Debug, Deserialize)]
pub(super) struct LabelConnection {
    pub nodes: Vec<GraphqlLabel>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlLabel {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct PageInfo {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub end_cursor: Option<String>,
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
                exit_code: 1,
                diagnostics: b"GraphQL: denied".to_vec(),
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
        assert_eq!(exchange.exit_code, 1);
        assert_eq!(exchange.diagnostics, b"GraphQL: denied");
        let envelope = envelope.unwrap();
        assert!(matches!(
            envelope.data,
            Some(IssueTypePageResponse { repository: None })
        ));
        let path = envelope.errors.into_iter().next().unwrap().path.unwrap();
        assert!(matches!(
            path.as_slice(),
            [GraphqlPathSegment::Field(field), GraphqlPathSegment::Index(0)]
                if field == "repository"
        ));
    }
}
