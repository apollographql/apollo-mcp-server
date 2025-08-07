use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error};

use super::Running;
use crate::server_config::ServerConfig;
use crate::server_handler::ApolloMcpServerHandler;
use crate::{errors::ServerError, operations::RawOperation};

pub(super) struct Starting {
    pub(super) config: ServerConfig,
    pub(super) schema: Valid<Schema>,
    pub(super) operations: Vec<RawOperation>,
}

impl Starting {
    pub(super) async fn start(
        self,
        server_handler: Arc<RwLock<ApolloMcpServerHandler>>,
    ) -> Result<Running, ServerError> {
        let operations: Vec<_> = self
            .operations
            .into_iter()
            .filter_map(|operation| {
                operation
                    .into_operation(
                        &self.schema,
                        self.config.custom_scalar_map.as_ref(),
                        self.config.mutation_mode,
                        self.config.disable_type_description,
                        self.config.disable_schema_description,
                    )
                    .unwrap_or_else(|error| {
                        error!("Invalid operation: {}", error);
                        None
                    })
            })
            .collect();

        debug!(
            "Loaded {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );

        server_handler
            .write()
            .await
            .configure(&self.config, self.schema.clone())?;
        let schema = Arc::new(Mutex::new(self.schema));

        let running = Running {
            schema,
            server_handler,
            custom_scalar_map: self.config.custom_scalar_map,
            mutation_mode: self.config.mutation_mode,
            disable_type_description: self.config.disable_type_description,
            disable_schema_description: self.config.disable_schema_description,
        };

        Ok(running)
    }
}
