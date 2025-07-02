use apollo_compiler::{Schema, validation::Valid};
use reqwest::header::HeaderMap;
use tracing::debug;

use crate::{
    custom_scalar_map::CustomScalarMap,
    errors::ServerError,
    operations::{MutationMode, RawOperation},
    server::Transport,
};

use super::Starting;

pub(super) struct SchemaConfigured {
    pub(super) transport: Transport,
    pub(super) schema: Valid<Schema>,
    pub(super) endpoint: String,
    pub(super) headers: HeaderMap,
    pub(super) introspection: bool,
    pub(super) explorer_graph_ref: Option<String>,
    pub(super) custom_scalar_map: Option<CustomScalarMap>,
    pub(super) mutation_mode: MutationMode,
    pub(super) disable_type_description: bool,
    pub(super) disable_schema_description: bool,
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
            transport: self.transport,
            schema: self.schema,
            operations,
            endpoint: self.endpoint,
            headers: self.headers,
            introspection: self.introspection,
            explorer_graph_ref: self.explorer_graph_ref,
            custom_scalar_map: self.custom_scalar_map,
            mutation_mode: self.mutation_mode,
            disable_type_description: self.disable_type_description,
            disable_schema_description: self.disable_schema_description,
        })
    }
}
