use std::collections::HashMap;
use std::path::Path;

use apollo_compiler::{Schema, validation::Valid};
use apollo_federation::{ApiSchemaOptions, Supergraph};
use apollo_mcp_registry::files;
use apollo_mcp_registry::uplink::schema::{SchemaState, event::Event as SchemaEvent};
use futures::{FutureExt as _, Stream, StreamExt as _, stream};
use reqwest::header::HeaderMap;
use tracing::info;
use url::Url;

use crate::{
    cors::CorsConfig,
    custom_scalar_map::CustomScalarMap,
    errors::{OperationError, ServerError},
    headers::ForwardHeaders,
    health::HealthCheckConfig,
    operations::MutationMode,
    server_info::ServerInfoConfig,
};

use super::{Server, ServerEvent, ShutdownReason, Transport};

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
    execute_tool_hint: Option<String>,
    introspect_tool_hint: Option<String>,
    search_tool_hint: Option<String>,
    validate_tool_hint: Option<String>,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
    enable_output_schema: bool,
    disable_auth_token_passthrough: bool,
    descriptions: HashMap<String, String>,
    required_scopes: HashMap<String, Vec<String>>,
    search_leaf_depth: usize,
    index_memory_bytes: usize,
    health_check: HealthCheckConfig,
    cors: CorsConfig,
    server_info: ServerInfoConfig,
}

impl StateMachine {
    pub(crate) async fn start(self, server: Server) -> Result<ShutdownReason, ServerError> {
        let schema_stream = server
            .schema_source
            .into_stream()
            .map(ServerEvent::SchemaUpdated)
            .boxed();
        let operation_stream = server.operation_source.into_stream().await.boxed();
        let ctrl_c_stream = Self::ctrl_c_stream().boxed();
        let rhai_stream = Self::rhai_watch_stream().boxed();
        let config_stream = Self::config_watch_stream(server.config_path.as_deref()).boxed();
        let sighup_stream = Self::sighup_stream().boxed();
        let mut stream = stream::select_all(vec![
            schema_stream,
            operation_stream,
            ctrl_c_stream,
            rhai_stream,
            config_stream,
            sighup_stream,
        ]);

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
                execute_tool_hint: server.execute_tool_hint,
                introspect_tool_hint: server.introspect_tool_hint,
                search_tool_hint: server.search_tool_hint,
                validate_tool_hint: server.validate_tool_hint,
                custom_scalar_map: server.custom_scalar_map,
                mutation_mode: server.mutation_mode,
                disable_type_description: server.disable_type_description,
                disable_schema_description: server.disable_schema_description,
                enable_output_schema: server.enable_output_schema,
                disable_auth_token_passthrough: server.disable_auth_token_passthrough,
                descriptions: server.descriptions,
                required_scopes: server.required_scopes,
                search_leaf_depth: server.search_leaf_depth,
                index_memory_bytes: server.index_memory_bytes,
                health_check: server.health_check,
                cors: server.cors,
                server_info: server.server_info,
            },
        });

        while let Some(event) = stream.next().await {
            state = Self::process_event(state, event).await?;
            if let State::Starting(starting) = state {
                state = starting.start().await.into();
            }
            if matches!(
                &state,
                State::Error(_) | State::Stopping | State::Restarting
            ) {
                break;
            }
        }
        match state {
            State::Error(e) => Err(e),
            State::Restarting => Ok(ShutdownReason::Restart),
            _ => Ok(ShutdownReason::Shutdown),
        }
    }

    /// Process a single event against the current state, returning the new state.
    #[allow(clippy::result_large_err)]
    async fn process_event(state: State, event: ServerEvent) -> Result<State, ServerError> {
        Ok(match event {
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
                        State::Running(running) => {
                            running.update_schema(schema).await;
                            running.into()
                        }
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
                State::Running(running) => {
                    running.update_operations(operations).await;
                    running.into()
                }
                other => other,
            },
            ServerEvent::OperationError(e, _) => {
                State::Error(ServerError::Operation(OperationError::File(e)))
            }
            ServerEvent::CollectionError(e) => match state {
                State::Running(running) => {
                    tracing::error!(
                        "Collection error while running, keeping existing operations: {e}"
                    );
                    running.into()
                }
                _ => State::Error(ServerError::Operation(OperationError::Collection(e))),
            },
            ServerEvent::RhaiScriptsChanged => match state {
                State::Running(running) => {
                    running.reload_rhai_scripts();
                    running.into()
                }
                other => other,
            },
            ServerEvent::ConfigChanged => {
                info!("Config file changed, triggering server restart");
                if let State::Running(ref running) = state {
                    running.cancellation_token.cancel();
                }
                State::Restarting
            }
            ServerEvent::Shutdown => match state {
                State::Running(running) => {
                    running.cancellation_token.cancel();
                    State::Stopping
                }
                _ => State::Stopping,
            },
        })
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

    /// Watch the config file for changes, mirroring the rhai_watch_stream pattern.
    /// Skips the initial event that files::watch always emits on startup.
    fn config_watch_stream(config_path: Option<&Path>) -> impl Stream<Item = ServerEvent> {
        match config_path {
            Some(path) if path.exists() => files::watch(path)
                .skip(1)
                .map(|_| ServerEvent::ConfigChanged)
                .boxed(),
            _ => stream::empty().boxed(),
        }
    }

    /// Listen for SIGHUP to trigger a config reload (Unix only).
    fn sighup_stream() -> impl Stream<Item = ServerEvent> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::SignalKind;

            async fn wait_for_sighup() -> Option<ServerEvent> {
                let mut signal = tokio::signal::unix::signal(SignalKind::hangup()).ok()?;
                signal.recv().await?;
                Some(ServerEvent::ConfigChanged)
            }

            wait_for_sighup()
                .map(stream::iter)
                .into_stream()
                .flatten()
                .boxed()
        }

        #[cfg(not(unix))]
        {
            stream::empty().boxed()
        }
    }

    fn rhai_watch_stream() -> impl Stream<Item = ServerEvent> {
        let rhai_dir = Path::new("rhai");

        // Limitation: the rhai directory must exist on startup for hot reloading to work.
        // If the user creates it after the fact, they will need to restart the server.
        if rhai_dir.is_dir() {
            files::watch_recursive(rhai_dir)
                .map(|_| ServerEvent::RhaiScriptsChanged)
                .boxed()
        } else {
            stream::empty().boxed()
        }
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
    Restarting,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use apollo_compiler::Schema;
    use apollo_mcp_registry::platform_api::operation_collections::error::CollectionError;
    use reqwest::header::HeaderMap;
    use tokio::sync::RwLock;
    use tokio_util::sync::CancellationToken;

    use crate::cors::CorsConfig;
    use crate::event::Event as ServerEvent;
    use crate::health::HealthCheckConfig;
    use crate::host_validation::HostValidationConfig;
    use crate::operations::{MutationMode, RawOperation};
    use crate::server::Transport;
    use crate::server_info::ServerInfoConfig;
    use apollo_mcp_rhai::RhaiEngine;

    use super::{Config, Configuring, Running, State, StateMachine};

    fn create_running_server() -> Running {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        Running {
            schema: Arc::new(RwLock::new(schema)),
            operations: Arc::new(RwLock::new(vec![])),
            apps: vec![],
            prompts: vec![],
            headers: HeaderMap::new(),
            forward_headers: vec![],
            endpoint: "http://localhost:4000".parse().unwrap(),
            execute_tool: None,
            introspect_tool: None,
            search_tool: None,
            explorer_tool: None,
            validate_tool: None,
            custom_scalar_map: None,
            peers: Arc::new(RwLock::new(vec![])),
            cancellation_token: CancellationToken::new(),
            mutation_mode: MutationMode::None,
            disable_type_description: false,
            disable_schema_description: false,
            enable_output_schema: false,
            disable_auth_token_passthrough: false,
            descriptions: HashMap::new(),
            health_check: None,
            server_info: ServerInfoConfig::default(),
            rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
        }
    }

    fn test_config() -> Config {
        Config {
            transport: Transport::StreamableHttp {
                auth: None,
                address: "127.0.0.1".parse().unwrap(),
                port: 0,
                stateful_mode: false,
                host_validation: HostValidationConfig::default(),
            },
            endpoint: "http://localhost:4000".parse().unwrap(),
            headers: HeaderMap::new(),
            forward_headers: vec![],
            execute_introspection: false,
            validate_introspection: false,
            introspect_introspection: false,
            search_introspection: false,
            introspect_minify: false,
            search_minify: false,
            explorer_graph_ref: None,
            execute_tool_hint: None,
            introspect_tool_hint: None,
            search_tool_hint: None,
            validate_tool_hint: None,
            custom_scalar_map: None,
            mutation_mode: MutationMode::None,
            disable_type_description: false,
            disable_schema_description: false,
            enable_output_schema: false,
            disable_auth_token_passthrough: false,
            descriptions: HashMap::new(),
            required_scopes: HashMap::new(),
            search_leaf_depth: 5,
            index_memory_bytes: 1024 * 1024,
            health_check: HealthCheckConfig::default(),
            cors: CorsConfig::default(),
            server_info: ServerInfoConfig::default(),
        }
    }

    async fn process_event(state: State, event: ServerEvent) -> State {
        StateMachine::process_event(state, event)
            .await
            .unwrap_or_else(State::Error)
    }

    #[tokio::test]
    async fn operations_updated_keeps_server_running() {
        let running = create_running_server();
        let state = State::Running(running);

        let event = ServerEvent::OperationsUpdated(vec![RawOperation::from((
            "query Valid { id }".to_string(),
            Some("valid.graphql".to_string()),
        ))]);

        let new_state = process_event(state, event).await;

        assert!(
            matches!(new_state, State::Running(_)),
            "expected server to remain Running after operations update"
        );
    }

    // A CollectionError while Running should NOT kill the server.
    // The server keeps its existing operations and stays alive.
    #[tokio::test]
    async fn collection_error_keeps_running_server_alive() {
        let running = create_running_server();
        let state = State::Running(running);

        let event = ServerEvent::CollectionError(CollectionError::InvalidVariables(
            r#"not valid json"#.to_string(),
        ));

        let new_state = process_event(state, event).await;

        assert!(
            matches!(new_state, State::Running(_)),
            "expected server to remain Running after CollectionError"
        );
    }

    // A CollectionError from a Platform API error while Running should also
    // keep the server alive.
    #[tokio::test]
    async fn collection_api_error_keeps_running_server_alive() {
        let running = create_running_server();
        let state = State::Running(running);

        let event =
            ServerEvent::CollectionError(CollectionError::Response("missing data".to_string()));

        let new_state = process_event(state, event).await;

        assert!(
            matches!(new_state, State::Running(_)),
            "expected server to remain Running after API collection error"
        );
    }

    // A RhaiScriptsChanged event while Running should NOT kill the server.
    #[tokio::test]
    async fn rhai_scripts_changed_keeps_running_server_alive() {
        let running = create_running_server();
        let state = State::Running(running);

        let event = ServerEvent::RhaiScriptsChanged;

        let new_state = process_event(state, event).await;

        assert!(
            matches!(new_state, State::Running(_)),
            "expected server to remain Running after RhaiScriptsChanged"
        );
    }

    // A CollectionError during startup (before Running) should still be fatal.
    #[tokio::test]
    async fn collection_error_during_startup_is_fatal() {
        let event = ServerEvent::CollectionError(CollectionError::InvalidVariables(
            r#"bad json"#.to_string(),
        ));

        let state = State::Configuring(Configuring {
            config: test_config(),
        });

        let new_state = process_event(state, event).await;

        assert!(
            matches!(new_state, State::Error(_)),
            "expected CollectionError during startup to be fatal"
        );
    }

    #[tokio::test]
    async fn config_changed_transitions_running_to_restarting() {
        let running = create_running_server();
        let state = State::Running(running);

        let new_state = process_event(state, ServerEvent::ConfigChanged).await;

        assert!(
            matches!(new_state, State::Restarting),
            "expected server to transition to Restarting after ConfigChanged"
        );
    }

    #[tokio::test]
    async fn config_changed_cancels_token_when_running() {
        let running = create_running_server();
        let token = running.cancellation_token.clone();
        let state = State::Running(running);

        assert!(!token.is_cancelled(), "token should not be cancelled yet");

        let _new_state = process_event(state, ServerEvent::ConfigChanged).await;

        assert!(
            token.is_cancelled(),
            "cancellation token should be cancelled after ConfigChanged"
        );
    }

    #[tokio::test]
    async fn config_changed_transitions_configuring_to_restarting() {
        let state = State::Configuring(Configuring {
            config: test_config(),
        });

        let new_state = process_event(state, ServerEvent::ConfigChanged).await;

        assert!(
            matches!(new_state, State::Restarting),
            "expected Configuring to transition to Restarting after ConfigChanged"
        );
    }

    #[tokio::test]
    async fn config_watch_stream_returns_empty_when_no_path() {
        use futures::StreamExt as _;

        let mut stream = StateMachine::config_watch_stream(None).boxed();

        assert!(
            futures::poll!(stream.next()).is_ready(),
            "expected empty stream to complete immediately"
        );
    }

    #[tokio::test]
    async fn config_watch_stream_returns_empty_for_nonexistent_path() {
        use futures::StreamExt as _;

        let path = std::path::Path::new("/tmp/nonexistent-config-file-12345.yaml");
        let mut stream = StateMachine::config_watch_stream(Some(path)).boxed();

        assert!(
            futures::poll!(stream.next()).is_ready(),
            "expected empty stream for nonexistent path"
        );
    }

    #[tokio::test]
    async fn shutdown_transitions_running_to_stopping() {
        let running = create_running_server();
        let token = running.cancellation_token.clone();
        let state = State::Running(running);

        let new_state = process_event(state, ServerEvent::Shutdown).await;

        assert!(
            matches!(new_state, State::Stopping),
            "expected Running to transition to Stopping after Shutdown"
        );
        assert!(
            token.is_cancelled(),
            "cancellation token should be cancelled after Shutdown"
        );
    }

    #[tokio::test]
    async fn shutdown_transitions_configuring_to_stopping() {
        let state = State::Configuring(Configuring {
            config: test_config(),
        });

        let new_state = process_event(state, ServerEvent::Shutdown).await;

        assert!(
            matches!(new_state, State::Stopping),
            "expected Configuring to transition to Stopping after Shutdown"
        );
    }
}
