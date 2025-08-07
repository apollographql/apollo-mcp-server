use apollo_compiler::{Schema, validation::Valid};
use tracing::debug;

use crate::server_config::ServerConfig;
use crate::{errors::ServerError, operations::RawOperation, server::states::Starting};

pub(super) struct OperationsConfigured {
    pub(super) config: ServerConfig,
    pub(super) operations: Vec<RawOperation>,
}

impl OperationsConfigured {
    pub(super) async fn set_schema(self, schema: Valid<Schema>) -> Result<Starting, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(Starting {
            config: self.config,
            operations: self.operations,
            schema,
        })
    }

    pub(super) async fn set_operations(
        self,
        operations: Vec<RawOperation>,
    ) -> Result<OperationsConfigured, ServerError> {
        debug!(
            "Received {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );
        Ok(OperationsConfigured { operations, ..self })
    }
}
