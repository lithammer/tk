//! GraphQL transport backed by `gh api graphql`.

use std::path::Path;

use crate::proc::ProcRunner;

use super::transport::{
    GraphqlCompleted, GraphqlCompletion, GraphqlExchange, GraphqlRequest, GraphqlStartFailure,
    GraphqlTransport,
};

/// GraphQL transport backed by the authenticated `gh` CLI.
pub(in crate::remote::github) struct CliGraphqlTransport<'a> {
    runner: &'a dyn ProcRunner,
    cwd: &'a Path,
}

impl<'a> CliGraphqlTransport<'a> {
    /// Bind GraphQL delivery to the shared subprocess seam and command cwd.
    pub(in crate::remote::github) fn new(runner: &'a dyn ProcRunner, cwd: &'a Path) -> Self {
        Self { runner, cwd }
    }
}

impl GraphqlTransport for CliGraphqlTransport<'_> {
    fn exchange(&self, request: &GraphqlRequest) -> GraphqlExchange {
        let body = request.body();
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
            Ok(output) => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                let completion = if output.succeeded() {
                    GraphqlCompletion::Succeeded { detail }
                } else {
                    GraphqlCompletion::Failed { detail }
                };
                GraphqlExchange::Completed(GraphqlCompleted {
                    body: output.stdout,
                    completion,
                })
            }
            Err(error @ crate::proc::ProcError::ExecutableNotFound) => {
                GraphqlExchange::NotStarted(GraphqlStartFailure::Unavailable(error.to_string()))
            }
            Err(error @ crate::proc::ProcError::SpawnFailed) => {
                GraphqlExchange::NotStarted(GraphqlStartFailure::Failed(error.to_string()))
            }
            Err(error @ crate::proc::ProcError::OutcomeUnobserved) => {
                GraphqlExchange::OutcomeUnobserved(error.to_string())
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
        assert!(matches!(
            completed.completion,
            GraphqlCompletion::Failed { ref detail } if detail == "GraphQL: denied"
        ));
        assert_eq!(
            completed.body,
            br#"{"data":{"viewer":null},"errors":[{"message":"denied"}]}"#
        );
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
