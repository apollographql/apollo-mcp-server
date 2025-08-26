use apollo_compiler::{Schema, validation::Valid};
use tracing::debug;

use super::{OperationsConfigured, SchemaConfigured};
use crate::server_config::ServerConfig;
use crate::{errors::ServerError, operations::RawOperation};

pub(super) struct Configuring {
    pub(super) config: ServerConfig,
}

impl Configuring {
    pub(super) async fn set_schema(
        self,
        schema: Valid<Schema>,
    ) -> Result<SchemaConfigured, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(SchemaConfigured {
            config: self.config,
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
        Ok(OperationsConfigured {
            config: self.config,
            operations,
        })
    }
}
