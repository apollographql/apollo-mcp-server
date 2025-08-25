use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use tokio::sync::{Mutex};
use tracing::{debug, error};

use crate::server_handler::{ApolloMcpServerHandler, McpServerHandler};
use crate::{
    custom_scalar_map::CustomScalarMap,
    errors::ServerError,
    operations::{MutationMode, Operation, RawOperation},
};

#[derive(Clone)]
pub struct Running<T = ApolloMcpServerHandler>
where
    T: McpServerHandler,
{
    pub(super) schema: Arc<Mutex<Valid<Schema>>>,
    pub(super) server_handler: T,
    pub(super) custom_scalar_map: Option<CustomScalarMap>,
    pub(super) mutation_mode: MutationMode,
    pub(super) disable_type_description: bool,
    pub(super) disable_schema_description: bool,
}

impl<T> Running<T>
where
    T: McpServerHandler,
{
    /// Update a running server with a new schema.
    pub(super) async fn update_schema(
        mut self,
        schema: Valid<Schema>,
    ) -> Result<Running<T>, ServerError> {
        debug!("Schema updated:\n{}", schema);

        // Update the operations based on the new schema. This is necessary because the MCP tool
        // input schemas and description are derived from the schema.
        let operations: Vec<Operation> = self
            .server_handler
            .operations()
            .await
            .iter()
            .cloned()
            .map(|operation| operation.into_inner())
            .filter_map(|operation| {
                operation
                    .into_operation(
                        &schema,
                        self.custom_scalar_map.as_ref(),
                        self.mutation_mode,
                        self.disable_type_description,
                        self.disable_schema_description,
                    )
                    .unwrap_or_else(|error| {
                        error!("Invalid operation: {}", error);
                        None
                    })
            })
            .collect();

        debug!(
            "Updated {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );
        self.server_handler.set_operations(operations).await;

        // Update the schema itself
        *self.schema.lock().await = schema;

        // Notify MCP clients that tools have changed
        self.server_handler
            .notify_tool_list_changed(self.server_handler.peers().await)
            .await;
        Ok(self)
    }

    pub(super) async fn update_operations(
        mut self,
        operations: Vec<RawOperation>,
    ) -> Result<Running<T>, ServerError> {
        debug!("Operations updated:\n{:?}", operations);

        // Update the operations based on the current schema
        {
            let schema = &*self.schema.lock().await;
            let updated_operations: Vec<Operation> = operations
                .into_iter()
                .filter_map(|operation| {
                    operation
                        .into_operation(
                            schema,
                            self.custom_scalar_map.as_ref(),
                            self.mutation_mode,
                            self.disable_type_description,
                            self.disable_schema_description,
                        )
                        .unwrap_or_else(|error| {
                            error!("Invalid operation: {}", error);
                            None
                        })
                })
                .collect();

            debug!(
                "Loaded {} operations:\n{}",
                updated_operations.len(),
                serde_json::to_string_pretty(&updated_operations)?
            );
            self.server_handler.set_operations(updated_operations).await;
        }

        // Notify MCP clients that tools have changed
        self.server_handler
            .notify_tool_list_changed(self.server_handler.peers().await)
            .await;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;
    use url::Url;

    // #[tokio::test]
    // async fn invalid_operations_should_not_crash_server() {
    //     let schema = Schema::parse("type Query { id: String }", "schema.graphql")
    //         .unwrap()
    //         .validate()
    //         .unwrap();
    //
    //     let server_handler = ApolloMcpServerHandler::new(
    //         HeaderMap::new(),
    //         Url::parse("http://localhost:8080/graphql").unwrap(),
    //     );
    //
    //     let running = Running {
    //         schema: Arc::new(Mutex::new(schema)),
    //         custom_scalar_map: None,
    //         mutation_mode: MutationMode::None,
    //         disable_type_description: false,
    //         disable_schema_description: false,
    //         server_handler: Arc::new(RwLock::new(server_handler)),
    //     };
    //
    //     let operations = vec![
    //         RawOperation::from((
    //             "query Valid { id }".to_string(),
    //             Some("valid.graphql".to_string()),
    //         )),
    //         RawOperation::from((
    //             "query Invalid {{ id }".to_string(),
    //             Some("invalid.graphql".to_string()),
    //         )),
    //         RawOperation::from((
    //             "query { id }".to_string(),
    //             Some("unnamed.graphql".to_string()),
    //         )),
    //     ];
    //
    //     let updated_running = running.update_operations(operations).await.unwrap();
    //     let updated_operations = updated_running.server_handler.read().await.operations();
    //     let operations_guard = updated_operations.lock().await;
    //
    //     assert_eq!(operations_guard.len(), 1);
    //     assert_eq!(operations_guard.first().unwrap().as_ref().name, "Valid");
    // }
}
