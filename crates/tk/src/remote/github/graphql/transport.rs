//! Replaceable wire transport for GitHub GraphQL operations (ADR-0042).

use serde::Serialize;
use serde_json::Value;

/// One standard GraphQL request, independent of its delivery mechanism.
pub(in crate::remote::github) struct GraphqlRequest {
    /// GitHub host that owns this operation's data.
    pub host: String,
    /// Operation name declared in the document.
    pub operation_name: &'static str,
    /// Complete GraphQL document for this operation.
    pub document: String,
    /// Variables whose names and shapes match the document.
    pub variables: Value,
}

impl GraphqlRequest {
    /// Encode the standard GraphQL JSON payload shared by every transport.
    pub(in crate::remote::github) fn body(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Body<'a> {
            query: &'a str,
            #[serde(rename = "operationName")]
            operation_name: &'a str,
            variables: &'a Value,
        }

        serde_json::to_vec(&Body {
            query: &self.document,
            operation_name: self.operation_name,
            variables: &self.variables,
        })
        .expect("GraphQL request values must serialize as JSON")
    }
}

/// Why a transport could not begin a GraphQL exchange.
pub(in crate::remote::github) enum GraphqlStartFailure {
    /// The configured transport is not available in this environment.
    Unavailable(String),
    /// The transport was available but could not start this exchange.
    Failed(String),
}

impl GraphqlStartFailure {
    /// Transport-owned detail for creation diagnostics.
    pub(in crate::remote::github) fn detail(&self) -> &str {
        match self {
            Self::Unavailable(detail) | Self::Failed(detail) => detail,
        }
    }
}

/// Delivery status for a response whose body was observed.
pub(in crate::remote::github) enum GraphqlCompletion {
    /// The transport reports a successful exchange.
    Succeeded { detail: String },
    /// The transport reports failure but still observed a response body.
    Failed { detail: String },
}

impl GraphqlCompletion {
    /// Transport failure detail, if delivery reported failure.
    pub(in crate::remote::github) fn failure_detail(&self) -> Option<&str> {
        match self {
            Self::Succeeded { .. } => None,
            Self::Failed { detail } => Some(detail),
        }
    }

    /// Transport detail observed alongside the response body.
    pub(in crate::remote::github) fn detail(&self) -> &str {
        match self {
            Self::Succeeded { detail } | Self::Failed { detail } => detail,
        }
    }
}

/// One response observed by the transport, including delivery status.
pub(in crate::remote::github) struct GraphqlCompleted {
    /// Raw response bytes retained for the operation's typed decoder.
    pub body: Vec<u8>,
    /// Delivery status retained separately from the response body.
    pub completion: GraphqlCompletion,
}

/// Transport-neutral evidence retained for each operation's failure policy.
pub(in crate::remote::github) enum GraphqlExchange {
    NotStarted(GraphqlStartFailure),
    Completed(GraphqlCompleted),
    OutcomeUnobserved(String),
}

/// Exchanges GraphQL requests while preserving transport evidence.
pub(in crate::remote::github) trait GraphqlTransport {
    /// Exchange one host-bound request and retain its effect certainty.
    fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange;
}
