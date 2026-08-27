//! `gh api graphql` transport adapter.

use std::path::Path;

use serde::Serialize;

use crate::proc::ProcRunner;

use super::transport::{
    GraphqlCompleted, GraphqlExchange, GraphqlRequest, GraphqlTransport, GraphqlTransportFailure,
};

/// GraphQL transport backed by the authenticated `gh` CLI.
pub(in crate::remote::github) struct CliGraphqlTransport<'a> {
    runner: &'a dyn ProcRunner,
    cwd: &'a Path,
}

impl<'a> CliGraphqlTransport<'a> {
    pub(in crate::remote::github) fn new(runner: &'a dyn ProcRunner, cwd: &'a Path) -> Self {
        Self { runner, cwd }
    }
}

impl GraphqlTransport for CliGraphqlTransport<'_> {
    fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
        #[derive(Serialize)]
        struct Body<'a> {
            query: &'a str,
            #[serde(rename = "operationName")]
            operation_name: &'a str,
            variables: &'a serde_json::Value,
        }

        let body = serde_json::to_vec(&Body {
            query: &request.document,
            operation_name: request.operation_name,
            variables: &request.variables,
        })
        .expect("GraphQL request values must serialize as JSON");
        let argv = [
            "gh",
            "api",
            "graphql",
            "--hostname",
            request.host.as_str(),
            "-H",
            "Content-Type: application/json",
            "--input",
            "-",
        ];
        match self.runner.run_with_stdin(&argv, self.cwd, &body) {
            Ok(output) => GraphqlExchange::Completed(GraphqlCompleted {
                body: output.stdout,
                exit_code: output.exit_code,
                diagnostics: output.stderr,
            }),
            Err(
                error @ (crate::proc::ProcError::ExecutableNotFound
                | crate::proc::ProcError::SpawnFailed),
            ) => GraphqlExchange::NotStarted(GraphqlTransportFailure {
                detail: error.to_string(),
                process_error: Some(error),
            }),
            Err(error @ crate::proc::ProcError::OutcomeUnobserved) => {
                GraphqlExchange::OutcomeUnobserved(GraphqlTransportFailure {
                    detail: error.to_string(),
                    process_error: Some(error),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::{FakeRunner, RunOutput};
    use serde_json::json;

    #[test]
    fn completed_nonzero_exchange_keeps_graphql_body_and_diagnostics() {
        let runner = FakeRunner::new();
        let body = br#"{"query":"query Viewer { viewer { login } }","operationName":"Viewer","variables":{}}"#;
        runner.expect_exact_with_stdin(
            &[
                "gh",
                "api",
                "graphql",
                "--hostname",
                "github.example.com",
                "-H",
                "Content-Type: application/json",
                "--input",
                "-",
            ],
            body,
            RunOutput {
                exit_code: 1,
                stdout: br#"{"data":{"viewer":null},"errors":[{"message":"denied"}]}"#.to_vec(),
                stderr: b"GraphQL: denied".to_vec(),
            },
        );
        let transport = CliGraphqlTransport::new(&runner, Path::new("."));

        let exchange = transport.exchange(&GraphqlRequest {
            host: "github.example.com".into(),
            operation_name: "Viewer",
            document: "query Viewer { viewer { login } }".into(),
            variables: json!({}),
        });

        let GraphqlExchange::Completed(completed) = exchange else {
            panic!("completed child must remain an observed exchange");
        };
        assert_eq!(completed.exit_code, 1);
        assert_eq!(
            completed.body,
            br#"{"data":{"viewer":null},"errors":[{"message":"denied"}]}"#
        );
        assert_eq!(completed.diagnostics, b"GraphQL: denied");
        runner.assert_all_consumed();
    }

    #[test]
    fn process_errors_keep_start_and_observation_evidence_distinct() {
        for (error, expected_not_started) in [
            (crate::proc::ProcError::SpawnFailed, true),
            (crate::proc::ProcError::OutcomeUnobserved, false),
        ] {
            let runner = FakeRunner::new();
            let body = br#"{"query":"query Viewer { viewer { login } }","operationName":"Viewer","variables":{}}"#;
            runner.expect_exact_with_stdin_error(
                &[
                    "gh",
                    "api",
                    "graphql",
                    "--hostname",
                    "github.example.com",
                    "-H",
                    "Content-Type: application/json",
                    "--input",
                    "-",
                ],
                body,
                error,
            );
            let transport = CliGraphqlTransport::new(&runner, Path::new("."));

            let exchange = transport.exchange(&GraphqlRequest {
                host: "github.example.com".into(),
                operation_name: "Viewer",
                document: "query Viewer { viewer { login } }".into(),
                variables: json!({}),
            });

            assert_eq!(
                matches!(exchange, GraphqlExchange::NotStarted(_)),
                expected_not_started
            );
            assert_eq!(
                matches!(exchange, GraphqlExchange::OutcomeUnobserved(_)),
                !expected_not_started
            );
            runner.assert_all_consumed();
        }
    }
}
