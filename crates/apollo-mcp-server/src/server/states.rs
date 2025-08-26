use apollo_compiler::{Schema, validation::Valid};
use apollo_federation::{ApiSchemaOptions, Supergraph};
use apollo_mcp_registry::uplink::schema::{SchemaState, event::Event as SchemaEvent};
use futures::{FutureExt as _, Stream, StreamExt as _, stream};
use std::sync::Arc;

use crate::errors::{OperationError, ServerError};

use super::{Server, ServerEvent};

mod configuring;
mod operations_configured;
mod running;
mod schema_configured;
mod starting;

use crate::server_handler::McpServerHandler;
use configuring::Configuring;
use operations_configured::OperationsConfigured;
use running::Running;
use schema_configured::SchemaConfigured;
use starting::Starting;

pub(super) struct StateMachine {}

impl StateMachine {
    pub(crate) async fn start<T: McpServerHandler + Clone>(
        self,
        server: Server<T>,
    ) -> Result<(), ServerError> {
        let schema_stream = server
            .schema_source
            .into_stream()
            .map(ServerEvent::SchemaUpdated)
            .boxed();
        let operation_stream = server.operation_source.into_stream().await.boxed();
        let ctrl_c_stream = Self::ctrl_c_stream().boxed();
        let mut stream = stream::select_all(vec![schema_stream, operation_stream, ctrl_c_stream]);

        let mut state = State::Configuring(Configuring {
            config: server.server_config,
        });

        while let Some(event) = stream.next().await {
            state = match event {
                ServerEvent::SchemaUpdated(registry_event) => match registry_event {
                    SchemaEvent::UpdateSchema(schema_state) => {
                        let schema = Self::sdl_to_api_schema(schema_state)?;
                        match state {
                            State::Configuring(configuring) => {
                                configuring.set_schema(schema).await.into()
                            }
                            State::SchemaConfigured(schema_configured) => {
                                schema_configured.set_schema(schema).await.into()
                            }
                            State::OperationsConfigured(operations_configured) => {
                                operations_configured.set_schema(schema).await.into()
                            }
                            State::Running(running) => running.update_schema(schema).await.into(),
                            other => other,
                        }
                    }
                    SchemaEvent::NoMoreSchema => match state {
                        State::Configuring(_) | State::OperationsConfigured(_) => {
                            State::Error(ServerError::NoSchema)
                        }
                        _ => state,
                    },
                },
                ServerEvent::OperationsUpdated(operations) => match state {
                    State::Configuring(configuring) => {
                        configuring.set_operations(operations).await.into()
                    }
                    State::SchemaConfigured(schema_configured) => {
                        schema_configured.set_operations(operations).await.into()
                    }
                    State::OperationsConfigured(operations_configured) => operations_configured
                        .set_operations(operations)
                        .await
                        .into(),
                    State::Running(running) => running.update_operations(operations).await.into(),
                    other => other,
                },
                ServerEvent::OperationError(e, _) => {
                    State::Error(ServerError::Operation(OperationError::File(e)))
                }
                ServerEvent::CollectionError(e) => {
                    State::Error(ServerError::Operation(OperationError::Collection(e)))
                }
                ServerEvent::Shutdown => match state {
                    State::Running(_running) => {
                        server.cancellation_token.cancel();
                        State::Stopping
                    }
                    _ => State::Stopping,
                },
            };
            if let State::Starting(starting) = state {
                // server
                //     .server_handler
                //     .write()
                //     .await
                //     .configure(&starting.config, starting.schema.clone())
                //     .await?;
                state = starting.start(server.server_handler.clone()).await.into();
            }
            if matches!(&state, State::Error(_) | State::Stopping) {
                break;
            }
        }
        match state {
            State::Error(e) => Err(e),
            _ => Ok(()),
        }
    }

    #[allow(clippy::result_large_err)]
    fn sdl_to_api_schema(schema_state: SchemaState) -> Result<Valid<Schema>, ServerError> {
        match Supergraph::new(&schema_state.sdl) {
            Ok(supergraph) => Ok(supergraph
                .to_api_schema(ApiSchemaOptions::default())
                .map_err(ServerError::Federation)?
                .schema()
                .clone()),
            Err(_) => Schema::parse_and_validate(schema_state.sdl, "schema.graphql")
                .map_err(|e| ServerError::GraphQLSchema(e.into())),
        }
    }

    fn ctrl_c_stream() -> impl Stream<Item = ServerEvent> {
        shutdown_signal()
            .map(|_| ServerEvent::Shutdown)
            .into_stream()
            .boxed()
    }
}

#[allow(clippy::expect_used)]
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[allow(clippy::large_enum_variant)]
enum State<T: McpServerHandler> {
    Configuring(Configuring),
    SchemaConfigured(SchemaConfigured),
    OperationsConfigured(OperationsConfigured),
    Starting(Starting),
    Running(Running<T>),
    Error(ServerError),
    Stopping,
}

impl<T: McpServerHandler> From<Configuring> for State<T> {
    fn from(starting: Configuring) -> Self {
        State::Configuring(starting)
    }
}

impl<T: McpServerHandler> From<SchemaConfigured> for State<T> {
    fn from(schema_configured: SchemaConfigured) -> Self {
        State::SchemaConfigured(schema_configured)
    }
}

impl<T: McpServerHandler> From<Result<SchemaConfigured, ServerError>> for State<T> {
    fn from(result: Result<SchemaConfigured, ServerError>) -> Self {
        match result {
            Ok(schema_configured) => State::SchemaConfigured(schema_configured),
            Err(error) => State::Error(error),
        }
    }
}

impl<T: McpServerHandler> From<OperationsConfigured> for State<T> {
    fn from(operations_configured: OperationsConfigured) -> Self {
        State::OperationsConfigured(operations_configured)
    }
}

impl<T: McpServerHandler> From<Result<OperationsConfigured, ServerError>> for State<T> {
    fn from(result: Result<OperationsConfigured, ServerError>) -> Self {
        match result {
            Ok(operations_configured) => State::OperationsConfigured(operations_configured),
            Err(error) => State::Error(error),
        }
    }
}

impl<T: McpServerHandler> From<Starting> for State<T> {
    fn from(starting: Starting) -> Self {
        State::Starting(starting)
    }
}

impl<T: McpServerHandler> From<Result<Starting, ServerError>> for State<T> {
    fn from(result: Result<Starting, ServerError>) -> Self {
        match result {
            Ok(starting) => State::Starting(starting),
            Err(error) => State::Error(error),
        }
    }
}

impl<T: McpServerHandler> From<Running<T>> for State<T> {
    fn from(running: Running<T>) -> Self {
        State::Running(running)
    }
}

impl<T: McpServerHandler> From<Result<Running<T>, ServerError>> for State<T> {
    fn from(result: Result<Running<T>, ServerError>) -> Self {
        match result {
            Ok(running) => State::Running(running),
            Err(error) => State::Error(error),
        }
    }
}

impl<T: McpServerHandler> From<ServerError> for State<T> {
    fn from(error: ServerError) -> Self {
        State::Error(error)
    }
}
