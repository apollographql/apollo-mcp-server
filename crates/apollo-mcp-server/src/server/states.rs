use apollo_compiler::{Schema, validation::Valid};
use apollo_federation::{ApiSchemaOptions, Supergraph};
use apollo_mcp_registry::uplink::schema::{SchemaState, event::Event as SchemaEvent};
use futures::{FutureExt as _, Stream, StreamExt as _, stream};
use reqwest::header::HeaderMap;
use url::Url;

use crate::{
    cors::CorsConfig,
    custom_scalar_map::CustomScalarMap,
    errors::{OperationError, ServerError},
    headers::ForwardHeaders,
    health::HealthCheckConfig,
    operations::MutationMode,
};

use super::{Server, ServerEvent, Transport};

mod configuring;
mod operations_configured;
mod running;
mod schema_configured;
mod starting;
mod telemetry;

use configuring::Configuring;
use operations_configured::OperationsConfigured;
use running::Running;
use schema_configured::SchemaConfigured;
use starting::Starting;

pub(super) struct StateMachine {}

/// Common configuration options for the states
struct Config {
    transport: Transport,
    endpoint: Url,
    headers: HeaderMap,
    forward_headers: ForwardHeaders,
    execute_introspection: bool,
    validate_introspection: bool,
    introspect_introspection: bool,
    search_introspection: bool,
    introspect_minify: bool,
    search_minify: bool,
    explorer_graph_ref: Option<String>,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
    disable_auth_token_passthrough: bool,
    search_leaf_depth: usize,
    index_memory_bytes: usize,
    health_check: HealthCheckConfig,
    cors: CorsConfig,
}

impl StateMachine {
    pub(crate) async fn start(self, server: Server) -> Result<(), ServerError> {
        let schema_stream = server
            .schema_source
            .into_stream()
            .map(ServerEvent::SchemaUpdated)
            .boxed();
        let operation_stream = server.operation_source.into_stream().await.boxed();
        let ctrl_c_stream = Self::ctrl_c_stream().boxed();
        let mut stream = stream::select_all(vec![schema_stream, operation_stream, ctrl_c_stream]);

        let mut state = State::Configuring(Configuring {
            config: Config {
                transport: server.transport,
                endpoint: server.endpoint,
                headers: server.headers,
                forward_headers: server.forward_headers,
                execute_introspection: server.execute_introspection,
                validate_introspection: server.validate_introspection,
                introspect_introspection: server.introspect_introspection,
                search_introspection: server.search_introspection,
                introspect_minify: server.introspect_minify,
                search_minify: server.search_minify,
                explorer_graph_ref: server.explorer_graph_ref,
                custom_scalar_map: server.custom_scalar_map,
                mutation_mode: server.mutation_mode,
                disable_type_description: server.disable_type_description,
                disable_schema_description: server.disable_schema_description,
                disable_auth_token_passthrough: server.disable_auth_token_passthrough,
                search_leaf_depth: server.search_leaf_depth,
                index_memory_bytes: server.index_memory_bytes,
                health_check: server.health_check,
                cors: server.cors,
            },
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
                    State::Running(running) => {
                        running.cancellation_token.cancel();
                        State::Stopping
                    }
                    _ => State::Stopping,
                },
            };
            if let State::Starting(starting) = state {
                state = starting.start().await.into();
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
        match Supergraph::new_with_router_specs(&schema_state.sdl) {
            Ok(supergraph) => Ok(supergraph
                .to_api_schema(ApiSchemaOptions::default())
                .map_err(|e| ServerError::Federation(Box::new(e)))?
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
async fn shutdown_signal() {
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
enum State {
    Configuring(Configuring),
    SchemaConfigured(SchemaConfigured),
    OperationsConfigured(OperationsConfigured),
    Starting(Starting),
    Running(Running),
    Error(ServerError),
    Stopping,
}

impl From<Configuring> for State {
    fn from(starting: Configuring) -> Self {
        State::Configuring(starting)
    }
}

impl From<SchemaConfigured> for State {
    fn from(schema_configured: SchemaConfigured) -> Self {
        State::SchemaConfigured(schema_configured)
    }
}

impl From<Result<SchemaConfigured, ServerError>> for State {
    fn from(result: Result<SchemaConfigured, ServerError>) -> Self {
        match result {
            Ok(schema_configured) => State::SchemaConfigured(schema_configured),
            Err(error) => State::Error(error),
        }
    }
}

impl From<OperationsConfigured> for State {
    fn from(operations_configured: OperationsConfigured) -> Self {
        State::OperationsConfigured(operations_configured)
    }
}

impl From<Result<OperationsConfigured, ServerError>> for State {
    fn from(result: Result<OperationsConfigured, ServerError>) -> Self {
        match result {
            Ok(operations_configured) => State::OperationsConfigured(operations_configured),
            Err(error) => State::Error(error),
        }
    }
}

impl From<Starting> for State {
    fn from(starting: Starting) -> Self {
        State::Starting(starting)
    }
}

impl From<Result<Starting, ServerError>> for State {
    fn from(result: Result<Starting, ServerError>) -> Self {
        match result {
            Ok(starting) => State::Starting(starting),
            Err(error) => State::Error(error),
        }
    }
}

impl From<Running> for State {
    fn from(running: Running) -> Self {
        State::Running(running)
    }
}

impl From<Result<Running, ServerError>> for State {
    fn from(result: Result<Running, ServerError>) -> Self {
        match result {
            Ok(running) => State::Running(running),
            Err(error) => State::Error(error),
        }
    }
}

impl From<ServerError> for State {
    fn from(error: ServerError) -> Self {
        State::Error(error)
    }
}
