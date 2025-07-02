use apollo_compiler::{Schema, validation::Valid};
use reqwest::header::HeaderMap;
use tracing::debug;

use crate::{
    custom_scalar_map::CustomScalarMap,
    errors::ServerError,
    operations::{MutationMode, RawOperation},
    server::Transport,
};

use super::{OperationsConfigured, SchemaConfigured};

pub(super) struct Configuring {
    pub(super) transport: Transport,
    pub(super) endpoint: String,
    pub(super) headers: HeaderMap,
    pub(super) introspection: bool,
    pub(super) explorer_graph_ref: Option<String>,
    pub(super) custom_scalar_map: Option<CustomScalarMap>,
    pub(super) mutation_mode: MutationMode,
    pub(super) disable_type_description: bool,
    pub(super) disable_schema_description: bool,
}

impl Configuring {
    pub(super) async fn set_schema(
        self,
        schema: Valid<Schema>,
    ) -> Result<SchemaConfigured, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(SchemaConfigured {
            transport: self.transport,
            schema,
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
            transport: self.transport,
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
