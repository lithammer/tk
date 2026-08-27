//! Replaceable wire transport for GitHub GraphQL operations (ADR-0042).

use serde_json::Value;

use crate::proc::ProcError;

/// One standard GraphQL request, independent of its delivery mechanism.
pub(in crate::remote::github) struct GraphqlRequest {
    pub host: String,
    pub operation_name: &'static str,
    pub document: String,
    pub variables: Value,
}

/// A failure before or during a GraphQL exchange.
pub(in crate::remote::github) struct GraphqlTransportFailure {
    pub detail: String,
    pub process_error: Option<ProcError>,
}

/// One terminal response observed by the transport.
pub(in crate::remote::github) struct GraphqlCompleted {
    pub body: Vec<u8>,
    pub exit_code: i32,
    pub diagnostics: Vec<u8>,
}

/// Transport evidence retained for operation-specific failure policy.
pub(in crate::remote::github) enum GraphqlExchange {
    NotStarted(GraphqlTransportFailure),
    Completed(GraphqlCompleted),
    OutcomeUnobserved(GraphqlTransportFailure),
}

/// Private delivery port implemented first by `gh api graphql`.
pub(in crate::remote::github) trait GraphqlTransport {
    fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange;
}
