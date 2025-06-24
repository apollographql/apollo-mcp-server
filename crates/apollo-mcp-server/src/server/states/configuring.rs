use apollo_compiler::{Schema, validation::Valid};
use rmcp::serde_json;
use tracing::debug;

use crate::{errors::ServerError, operations::RawOperation};

use super::{Config, OperationsConfigured, SchemaConfigured};

pub(super) struct Configuring {
    pub(super) config: Config,
}

impl Configuring {
    pub async fn set_schema(self, schema: Valid<Schema>) -> Result<SchemaConfigured, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(SchemaConfigured {
            config: self.config,
            schema,
        })
    }

    pub async fn set_operations(
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
