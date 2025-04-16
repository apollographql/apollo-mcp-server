#![deny(clippy::expect_used)]
#![deny(clippy::unwrap_used)]

pub mod operations;
pub mod server;

/// A list of GraphQL operations
pub(crate) type OperationsList = Vec<Operation>;

/// A GraphQL Operation
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Operation {
    pub query: String,
}
