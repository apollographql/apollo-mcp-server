use apollo_compiler::{Schema, validation::Valid};
use tracing::debug;

use crate::{errors::ServerError, operations::RawOperation};
use crate::server_config::ServerConfig;
use super::Starting;

pub(super) struct SchemaConfigured {
    pub(super) config: ServerConfig,
    pub(super) schema: Valid<Schema>,
}

impl SchemaConfigured {
    pub(super) async fn set_schema(
        self,
        schema: Valid<Schema>,
    ) -> Result<SchemaConfigured, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(SchemaConfigured { schema, ..self })
    }

    pub(super) async fn set_operations(
        self,
        operations: Vec<RawOperation>,
    ) -> Result<Starting, ServerError> {
        debug!(
            "Received {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );
        Ok(Starting {
            config: self.config,
            schema: self.schema,
            operations,
        })
    }
}
