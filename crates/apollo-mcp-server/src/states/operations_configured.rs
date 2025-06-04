use apollo_compiler::{Schema, validation::Valid};
use reqwest::header::HeaderMap;
use rmcp::serde_json;
use tracing::debug;

use crate::{
    custom_scalar_map::CustomScalarMap,
    errors::ServerError,
    operations::{MutationMode, RawOperation},
    server::Transport,
};

use super::Starting;

pub(crate) struct OperationsConfigured {
    pub(crate) transport: Transport,
    pub(crate) operations: Vec<RawOperation>,
    pub(crate) endpoint: String,
    pub(crate) headers: HeaderMap,
    pub(crate) introspection: bool,
    pub(crate) explorer: bool,
    pub(crate) custom_scalar_map: Option<CustomScalarMap>,
    pub(crate) mutation_mode: MutationMode,
    pub(crate) disable_type_description: bool,
    pub(crate) disable_schema_description: bool,
}

impl OperationsConfigured {
    pub(crate) async fn set_schema(self, schema: Valid<Schema>) -> Result<Starting, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(Starting {
            transport: self.transport,
            schema,
            operations: self.operations,
            endpoint: self.endpoint,
            headers: self.headers,
            introspection: self.introspection,
            explorer: self.explorer,
            custom_scalar_map: self.custom_scalar_map,
            mutation_mode: self.mutation_mode,
            disable_type_description: self.disable_type_description,
            disable_schema_description: self.disable_schema_description,
        })
    }

    pub(crate) async fn set_operations(
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
