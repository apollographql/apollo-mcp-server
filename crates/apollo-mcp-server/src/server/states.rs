use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use apollo_compiler::{Schema, validation::Valid};
use apollo_federation::{ApiSchemaOptions, Supergraph};
use apollo_mcp_registry::files;
use apollo_mcp_registry::uplink::schema::{SchemaState, event::Event as SchemaEvent};
use futures::stream::{BoxStream, SelectAll};
use futures::{FutureExt as _, Stream, StreamExt as _, stream};
use reqwest::header::HeaderMap;
use tracing::{error, info, warn};
use url::Url;

use crate::{
    caching::Caching,
    cors::CorsConfig,
    custom_scalar_map::CustomScalarMap,
    errors::{OperationError, ServerError},
    headers::ForwardHeaders,
    health::HealthCheckConfig,
    operations::{AnnotationOverrides, MutationMode, ParamHeaders},
    scope_requirements::OperationRequiredScopes,
    server_info::ServerInfoConfig,
};

use super::{ConfigReloader, Server, ServerEvent, Transport};

mod configuring;
mod operations_configured;
mod running;
mod schema_configured;
#[cfg(test)]
mod server_span_tests;
mod starting;
mod tasks;
pub(crate) mod telemetry;

pub use running::{MCP_SERVER_OPERATION_DURATION, MCP_SERVER_OPERATION_DURATION_KEYS};
mod tool_list_changes;

use configuring::Configuring;
use operations_configured::OperationsConfigured;
use running::Running;
use schema_configured::SchemaConfigured;
use starting::{Ready, Starting};

pub(super) struct StateMachine {
    events: SelectAll<BoxStream<'static, ServerEvent>>,
    state: State,
}

/// Servers built from each configuration change, ready to replace the running one.
pub(super) type Reloads = BoxStream<'static, StateMachine>;

/// Common configuration options for the states
struct Config {
    rhai_dir: PathBuf,
    apps_dir: PathBuf,
    expose_without_app_param: crate::apps::AppAllowlist,
    prompts_dir: PathBuf,
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
    gen_ai_content: bool,
    client_address: bool,
    descriptions: HashMap<String, String>,
    annotations: HashMap<String, AnnotationOverrides>,
    param_headers: HashMap<String, ParamHeaders>,
    required_scopes: HashMap<String, OperationRequiredScopes>,
    search_leaf_depth: usize,
    index_memory_bytes: usize,
    health_check: HealthCheckConfig,
    cors: CorsConfig,
    server_info: ServerInfoConfig,
    instructions: Option<String>,
    caching: Caching,
    tasks: crate::tasks::TasksConfig,
}

impl StateMachine {
    pub(super) async fn new(server: Server) -> Self {
        let schema_stream = server
            .schema_source
            .into_stream()
            .map(ServerEvent::SchemaUpdated)
            .boxed();
        let operation_stream = server.operation_source.into_stream().await.boxed();
        let ctrl_c_stream = Self::ctrl_c_stream().boxed();
        let rhai_stream = Self::rhai_watch_stream(&server.rhai_dir).boxed();
        let events = stream::select_all(vec![
            schema_stream,
            operation_stream,
            ctrl_c_stream,
            rhai_stream,
        ]);

        let state = State::Configuring(Configuring {
            config: Config {
                rhai_dir: server.rhai_dir,
                apps_dir: server.apps_dir,
                expose_without_app_param: server.expose_without_app_param,
                prompts_dir: server.prompts_dir,
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
                gen_ai_content: server.gen_ai_content,
                client_address: server.client_address,
                descriptions: server.descriptions,
                annotations: server.annotations,
                param_headers: server.param_headers,
                required_scopes: server.required_scopes,
                search_leaf_depth: server.search_leaf_depth,
                index_memory_bytes: server.index_memory_bytes,
                health_check: server.health_check,
                cors: server.cors,
                server_info: server.server_info,
                instructions: server.instructions,
                caching: server.caching,
                tasks: server.tasks,
            },
        });

        Self { events, state }
    }

    /// Builds a server from each configuration change and runs its startup. A change that
    /// fails is logged and skipped, so the running server keeps its configuration.
    pub(super) fn reloads(config_path: Option<&Path>, reloader: ConfigReloader) -> Reloads {
        let changes = stream::select(
            Self::config_watch_stream(config_path),
            Self::sighup_stream(),
        );
        Self::reloads_on(changes.boxed(), reloader)
    }

    fn reloads_on(changes: BoxStream<'static, ()>, reloader: ConfigReloader) -> Reloads {
        stream::unfold(changes, move |mut changes| {
            let reloader = reloader.clone();
            async move {
                loop {
                    changes.next().await?;
                    // The newest change wins, so a startup that never finishes can't hold
                    // back a corrected configuration.
                    loop {
                        tokio::select! {
                            biased;
                            Some(()) = changes.next() => {}
                            next = Self::reload(&reloader) => match next {
                                Some(next) => return Some((next, changes)),
                                None => break,
                            },
                        }
                    }
                }
            }
        })
        // `run` polls this again after it ends, which `unfold` alone doesn't allow.
        .fuse()
        .boxed()
    }

    async fn reload(reloader: &ConfigReloader) -> Option<Self> {
        let server = reloader()
            .inspect_err(|e| {
                error!("Config changed but contains errors, keeping current configuration: {e}")
            })
            .ok()?;
        let next = Self::new(server).await.prepare().await;
        match &next.state {
            State::Ready(_) => Some(next),
            State::Error(e) => {
                error!(
                    "Config changed but the server can't start with it, keeping current configuration: {e}"
                );
                None
            }
            // Startup was interrupted, for example by a shutdown signal.
            _ => None,
        }
    }

    /// Runs startup until the server is ready to serve, or startup ends another way.
    async fn prepare(mut self) -> Self {
        while matches!(
            self.state,
            State::Configuring(_) | State::SchemaConfigured(_) | State::OperationsConfigured(_)
        ) {
            let Some(event) = self.events.next().await else {
                break;
            };
            self = self.advance(event).await;
        }
        self
    }

    pub(super) async fn run(mut self, mut reloads: Reloads) -> Result<(), ServerError> {
        loop {
            if let State::Ready(ready) = self.state {
                self.state = ready.serve().await.into();
            }
            match self.state {
                State::Error(e) => return Err(e),
                State::Stopping => return Ok(()),
                _ => {}
            }
            tokio::select! {
                event = self.events.next() => {
                    let Some(event) = event else {
                        return Ok(());
                    };
                    self = self.advance(event).await;
                }
                Some(next) = reloads.next() => {
                    info!("Config changed, restarting server...");
                    warn!(
                        "Logging and telemetry configuration changes require a full process restart to take effect"
                    );
                    self.advance(ServerEvent::Shutdown).await;
                    // Brief delay to let the port be released
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    self = next;
                }
            }
        }
    }

    async fn advance(mut self, event: ServerEvent) -> Self {
        let tasks = match &self.state {
            State::Running(running) => running.tasks.clone(),
            _ => None,
        };
        let mut next = Self::process_event(self.state, event)
            .await
            .unwrap_or_else(State::Error);
        // Every exit from Running, including errors, ends its tasks.
        if !matches!(&next, State::Running(_))
            && let Some(tasks) = tasks
        {
            tasks.shutdown().await;
        }
        if let State::Starting(starting) = next {
            next = starting.prepare().await.into();
        }
        self.state = next;
        self
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
            ServerEvent::ManifestError(e) => match state {
                State::Running(running) => {
                    tracing::warn!(
                        "Manifest error while running, keeping existing operations: {e}"
                    );
                    running.into()
                }
                _ => State::Error(ServerError::Operation(OperationError::Manifest(e))),
            },
            ServerEvent::RhaiScriptsChanged => match state {
                State::Running(running) => {
                    running.reload_rhai_scripts();
                    running.into()
                }
                other => other,
            },
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
            // apollo-federation is still on apollo-compiler 1.x, so re-parse its API schema
            Ok(supergraph) => {
                let api_schema = supergraph
                    .to_api_schema(ApiSchemaOptions::default())
                    .map_err(|e| ServerError::Federation(Box::new(e)))?
                    .schema()
                    .to_string();
                let schema = Schema::parse(api_schema, "<API schema derived from supergraph>")
                    .map_err(|e| ServerError::ApiSchema(e.into()))?;
                // Federation already validated this schema, so only warn about newer rules
                Ok(schema.validate().unwrap_or_else(|invalid| {
                    warn!(
                        "The API schema derived from the supergraph breaks GraphQL September 2025 validation rules:\n{}",
                        invalid.errors
                    );
                    Valid::assume_valid(invalid.partial)
                }))
            }
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

    /// Watch the config file for changes, mirroring the Rhai watch stream pattern.
    /// Skips the initial event that files::watch always emits on startup.
    fn config_watch_stream(config_path: Option<&Path>) -> impl Stream<Item = ()> + use<> {
        match config_path {
            Some(path) if path.exists() => files::watch(path).skip(1).boxed(),
            _ => stream::empty().boxed(),
        }
    }

    /// Listen for SIGHUP to trigger a config reload (Unix only).
    fn sighup_stream() -> impl Stream<Item = ()> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            match signal(SignalKind::hangup()) {
                Ok(hangup) => stream::unfold(hangup, |mut hangup| async move {
                    hangup.recv().await.map(|()| ((), hangup))
                })
                .boxed(),
                Err(e) => {
                    error!("Failed to install SIGHUP handler: {e}");
                    stream::empty().boxed()
                }
            }
        }

        #[cfg(not(unix))]
        {
            stream::empty().boxed()
        }
    }

    fn rhai_watch_stream(rhai_dir: &Path) -> impl Stream<Item = ServerEvent> + use<> {
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
    Ready(Ready),
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

impl From<Result<Ready, ServerError>> for State {
    fn from(result: Result<Ready, ServerError>) -> Self {
        match result {
            Ok(ready) => State::Ready(ready),
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
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;

    use apollo_compiler::Schema;
    use apollo_mcp_registry::platform_api::operation_collections::error::CollectionError;
    use apollo_mcp_registry::uplink::schema::{SchemaSource, SchemaState};
    use assert_fs::{TempDir, prelude::*};
    use futures::{StreamExt as _, stream};
    use reqwest::header::HeaderMap;
    use tokio::sync::RwLock;

    use tokio_util::sync::CancellationToken;
    use tracing_test::traced_test;

    use crate::caching::Caching;
    use crate::cors::CorsConfig;
    use crate::errors::ServerError;
    use crate::event::Event as ServerEvent;
    use crate::health::HealthCheckConfig;
    use crate::host_validation::HostValidationConfig;
    use crate::operations::{MutationMode, OperationSource, RawOperation};
    use crate::server::{ConfigReloader, Server, Transport};
    use crate::server_info::ServerInfoConfig;
    use apollo_mcp_rhai::SharedRhaiEngine;

    use super::{Config, Configuring, Reloads, Running, State, StateMachine};

    fn create_running_server() -> Running {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        Running {
            schema: Arc::new(RwLock::new(schema)),
            operations: Arc::new(RwLock::new(vec![])),
            exports: Default::default(),
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
            tool_list_changes: Default::default(),
            cancellation_token: CancellationToken::new(),
            mutation_mode: MutationMode::None,
            disable_type_description: false,
            disable_schema_description: false,
            enable_output_schema: false,
            gen_ai_content: false,
            disable_auth_token_passthrough: false,
            descriptions: HashMap::new(),
            annotations: HashMap::new(),
            param_headers: HashMap::new(),
            health_check: None,
            server_info: ServerInfoConfig::default(),
            instructions: None,
            rhai_engine: SharedRhaiEngine::new("rhai"),
            caching: Caching::default(),
            tasks: Default::default(),
        }
    }

    fn test_config() -> Config {
        Config {
            rhai_dir: PathBuf::from("rhai"),
            expose_without_app_param: Default::default(),
            apps_dir: PathBuf::from("apps"),
            prompts_dir: PathBuf::from("prompts"),
            transport: Transport::StreamableHttp {
                auth: None,
                address: "127.0.0.1".parse().unwrap(),
                port: 0,
                path: Default::default(),
                stateful_mode: false,
                host_validation: HostValidationConfig::default(),
                trusted_proxies: Default::default(),
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
            gen_ai_content: false,
            client_address: false,
            disable_auth_token_passthrough: false,
            descriptions: HashMap::new(),
            annotations: HashMap::new(),
            param_headers: HashMap::new(),
            required_scopes: HashMap::new(),
            search_leaf_depth: 5,
            index_memory_bytes: 1024 * 1024,
            health_check: HealthCheckConfig::default(),
            cors: CorsConfig::default(),
            server_info: ServerInfoConfig::default(),
            instructions: None,
            caching: Caching::default(),
            tasks: Default::default(),
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

    // A ManifestError while Running should NOT kill the server or clear the catalog.
    #[tokio::test]
    async fn manifest_error_keeps_running_server_alive_and_retains_catalog() {
        let running = create_running_server();
        let state = State::Running(running);

        // Populate the catalog first so we can assert it is retained.
        let state = process_event(
            state,
            ServerEvent::OperationsUpdated(vec![RawOperation::from((
                "query GetId { id }".to_string(),
                Some("test.graphql".to_string()),
            ))]),
        )
        .await;

        let new_state = process_event(
            state,
            ServerEvent::ManifestError(
                "connection timeout fetching persisted query manifest".into(),
            ),
        )
        .await;

        let State::Running(running) = new_state else {
            panic!("expected server to remain Running after ManifestError");
        };
        assert_eq!(
            running.operations.read().await.len(),
            1,
            "ManifestError must not clear the existing tool catalog"
        );
    }

    // A ManifestError during startup (before Running) should be fatal.
    #[tokio::test]
    async fn manifest_error_during_startup_is_fatal() {
        let event = ServerEvent::ManifestError("timeout on first fetch".into());

        let state = State::Configuring(Configuring {
            config: test_config(),
        });

        let new_state = process_event(state, event).await;

        assert!(
            matches!(new_state, State::Error(_)),
            "expected ManifestError during startup to be fatal"
        );
    }

    // Contrast with ManifestError: an authoritative empty manifest (HTTP 200 with no
    // operations) arrives as OperationsUpdated([]) and SHOULD clear the operations.
    #[tokio::test]
    async fn empty_operations_update_clears_operations_while_running() {
        let running = create_running_server();
        let state = State::Running(running);

        // Load one operation, then deliver an authoritative empty update.
        let state = process_event(
            state,
            ServerEvent::OperationsUpdated(vec![RawOperation::from((
                "query GetId { id }".to_string(),
                Some("test.graphql".to_string()),
            ))]),
        )
        .await;

        let new_state = process_event(state, ServerEvent::OperationsUpdated(vec![])).await;

        let State::Running(running) = new_state else {
            panic!("expected server to remain Running after an intentional empty update");
        };
        assert!(
            running.operations.read().await.is_empty(),
            "an authoritative empty manifest should clear the operations"
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

    /// A server with one app, `records`, whose only tool is `Lookup`, and which exposes
    /// `exposed_tool` of that app without the app parameter.
    fn server_exposing(exposed_tool: &str) -> (Server, TempDir) {
        let apps_dir = TempDir::new().unwrap();
        let manifest = serde_json::json!({
            "format": "apollo-ai-app-manifest",
            "version": "1",
            "hash": "abcdef",
            "resource": "https://example.com/widget/index.html",
            "operations": [{
                "body": "query Hello { hello }",
                "tools": [{ "name": "Lookup", "description": "Says hello" }],
            }],
        });
        apps_dir
            .child("records")
            .child(".application-manifest.json")
            .write_str(&manifest.to_string())
            .unwrap();
        let server = Server::builder()
            .rhai_dir(PathBuf::from("rhai"))
            .apps_dir(apps_dir.path().into())
            .expose_without_app_param(
                [("records".to_owned(), vec![exposed_tool.to_owned()])].into(),
            )
            .transport(Transport::StreamableHttp {
                auth: None,
                address: "127.0.0.1".parse().unwrap(),
                port: 0,
                path: Default::default(),
                stateful_mode: false,
                host_validation: HostValidationConfig::default(),
                trusted_proxies: Default::default(),
            })
            .schema_source(SchemaSource::from("type Query { hello: String }"))
            .operation_source(OperationSource::None)
            .endpoint("http://localhost:4000".parse().unwrap())
            .headers(HeaderMap::new())
            .forward_headers(vec![])
            .execute_introspection(false)
            .validate_introspection(false)
            .introspect_introspection(false)
            .search_introspection(false)
            .introspect_minify(false)
            .search_minify(false)
            .custom_scalar_map(None)
            .mutation_mode(MutationMode::None)
            .disable_type_description(false)
            .disable_schema_description(false)
            .enable_output_schema(false)
            .disable_auth_token_passthrough(false)
            .descriptions(HashMap::new())
            .annotations(HashMap::new())
            .required_scopes(HashMap::<String, Vec<String>>::new())
            .search_leaf_depth(5)
            .index_memory_bytes(1024 * 1024)
            .health_check(HealthCheckConfig::default())
            .cors(CorsConfig::default())
            .server_info(ServerInfoConfig::default())
            .build();
        (server, apps_dir)
    }

    /// A reloader that hands out `server` once.
    fn reloader_for(server: Server) -> ConfigReloader {
        reloader_for_each([server])
    }

    /// A reloader that hands out `servers` in order, one per call.
    fn reloader_for_each(servers: impl IntoIterator<Item = Server>) -> ConfigReloader {
        let servers = std::sync::Mutex::new(servers.into_iter().collect::<VecDeque<_>>());
        Arc::new(move || Ok(servers.lock().unwrap().pop_front().unwrap()))
    }

    #[tokio::test]
    async fn reload_returns_a_server_that_is_ready_to_serve() {
        let (server, _apps_dir) = server_exposing("Lookup");

        let reloaded = StateMachine::reload(&reloader_for(server)).await;

        assert!(
            matches!(
                reloaded,
                Some(StateMachine {
                    state: State::Ready(_),
                    ..
                })
            ),
            "expected a server that passed startup"
        );
    }

    #[tokio::test]
    async fn reload_skips_a_configuration_that_fails_startup() {
        let (server, _apps_dir) = server_exposing("Lokup");

        let reloaded = StateMachine::reload(&reloader_for(server)).await;

        assert!(
            reloaded.is_none(),
            "expected the misspelled tool name to fail startup"
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn reload_logs_why_a_configuration_fails_startup() {
        let (server, _apps_dir) = server_exposing("Lokup");

        StateMachine::reload(&reloader_for(server)).await;

        assert!(logs_contain(
            "keeping current configuration: Failed to load apps: apps.expose_without_app_param: records has 0 tools named Lokup"
        ));
    }

    #[tokio::test]
    async fn reload_skips_a_configuration_that_fails_to_build() {
        let reloader: ConfigReloader = Arc::new(|| Err("invalid YAML".into()));

        let reloaded = StateMachine::reload(&reloader).await;

        assert!(reloaded.is_none(), "expected the build error to be skipped");
    }

    #[tokio::test]
    async fn reloads_drops_a_startup_that_never_finishes_when_the_configuration_changes_again() {
        let (mut stuck, _stuck_apps_dir) = server_exposing("Lookup");
        stuck.schema_source = SchemaSource::Stream(stream::pending().boxed());
        let (fixed, _fixed_apps_dir) = server_exposing("Lookup");
        let (changed, changes) = futures::channel::mpsc::unbounded();
        let mut reloads =
            StateMachine::reloads_on(changes.boxed(), reloader_for_each([stuck, fixed]));
        changed.unbounded_send(()).unwrap();
        assert!(futures::poll!(reloads.next()).is_pending());

        changed.unbounded_send(()).unwrap();
        let reloaded = tokio::time::timeout(Duration::from_secs(5), reloads.next()).await;

        assert!(
            matches!(
                reloaded,
                Ok(Some(StateMachine {
                    state: State::Ready(_),
                    ..
                }))
            ),
            "expected the second change to replace the stuck startup"
        );
    }

    #[tokio::test]
    async fn reloads_can_be_polled_again_after_the_changes_end() {
        let reloader: ConfigReloader = Arc::new(|| Err("unused".into()));
        let mut reloads = StateMachine::reloads_on(stream::empty().boxed(), reloader);
        reloads.next().await;

        assert!(reloads.next().await.is_none());
    }

    /// A server that is running and receives no events.
    fn idle_running_machine() -> (StateMachine, CancellationToken) {
        let running = create_running_server();
        let token = running.cancellation_token.clone();
        let machine = StateMachine {
            events: stream::select_all(vec![stream::pending().boxed()]),
            state: State::Running(running),
        };
        (machine, token)
    }

    /// A reloaded server that stops as soon as it takes over, so `run` returns.
    fn reloaded_machine_that_stops() -> Reloads {
        stream::iter([StateMachine {
            events: stream::select_all(vec![]),
            state: State::Stopping,
        }])
        .boxed()
    }

    #[tokio::test(start_paused = true)]
    async fn run_hands_over_to_a_reloaded_server() {
        let (machine, _token) = idle_running_machine();

        let result = machine.run(reloaded_machine_that_stops()).await;

        assert!(
            result.is_ok(),
            "expected the reloaded server to take over and stop: {:?}",
            result.err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn run_stops_the_running_server_when_a_reloaded_one_takes_over() {
        let (machine, token) = idle_running_machine();

        let _ = machine.run(reloaded_machine_that_stops()).await;

        assert!(
            token.is_cancelled(),
            "expected the replaced server's connections to be cancelled"
        );
    }

    #[tokio::test]
    async fn run_keeps_the_running_server_without_a_reload() {
        let (machine, token) = idle_running_machine();

        let result = tokio::time::timeout(
            Duration::from_millis(50),
            machine.run(stream::empty().boxed()),
        )
        .await;

        assert!(
            result.is_err() && !token.is_cancelled(),
            "expected the server to keep running"
        );
    }

    #[tokio::test]
    async fn config_watch_stream_returns_empty_when_no_path() {
        let mut stream = StateMachine::config_watch_stream(None).boxed();

        assert!(
            futures::poll!(stream.next()).is_ready(),
            "expected empty stream to complete immediately"
        );
    }

    #[tokio::test]
    async fn config_watch_stream_returns_empty_for_nonexistent_path() {
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

    const MINIMAL_SUPERGRAPH: &str = include_str!("testdata/minimal_supergraph.graphql");

    const RICH_SUPERGRAPH: &str = include_str!("testdata/rich_supergraph.graphql");

    fn schema_state(sdl: &str) -> SchemaState {
        SchemaState::from_str(sdl).unwrap()
    }

    #[test]
    fn sdl_to_api_schema_strips_join_types_from_supergraph() {
        let schema = StateMachine::sdl_to_api_schema(schema_state(MINIMAL_SUPERGRAPH)).unwrap();

        assert!(
            !schema.types.contains_key("join__Graph"),
            "expected the API schema to drop federation's join__Graph enum"
        );
    }

    #[test]
    fn sdl_to_api_schema_keeps_root_fields_of_supergraph() {
        let schema = StateMachine::sdl_to_api_schema(schema_state(MINIMAL_SUPERGRAPH)).unwrap();

        assert!(
            schema.type_field("Query", "me").is_ok(),
            "expected Query.me to survive the API schema re-parse"
        );
    }

    #[test]
    fn sdl_to_api_schema_keeps_rich_supergraph_constructs() {
        let schema = StateMachine::sdl_to_api_schema(schema_state(RICH_SUPERGRAPH)).unwrap();

        insta::assert_snapshot!("rich_supergraph_api_schema", schema.to_string());
    }

    #[traced_test]
    #[test]
    fn sdl_to_api_schema_validates_rich_supergraph_without_warnings() {
        StateMachine::sdl_to_api_schema(schema_state(RICH_SUPERGRAPH)).unwrap();

        assert!(!logs_contain(
            "breaks GraphQL September 2025 validation rules"
        ));
    }

    /// A supergraph federation accepts, but whose `@deprecated` on a required argument breaks a
    /// September 2025 validation rule
    fn supergraph_breaking_newer_rules() -> String {
        MINIMAL_SUPERGRAPH.replace(
            "me: String @join__field",
            "me(id: ID! @deprecated(reason: \"unused\")): String @join__field",
        )
    }

    #[test]
    fn sdl_to_api_schema_loads_derived_api_schema_that_breaks_newer_rules() {
        let result =
            StateMachine::sdl_to_api_schema(schema_state(&supergraph_breaking_newer_rules()));

        assert!(
            result.is_ok(),
            "expected the derived API schema to load, got {:?}",
            result.err()
        );
    }

    #[traced_test]
    #[test]
    fn sdl_to_api_schema_warns_when_derived_api_schema_breaks_newer_rules() {
        StateMachine::sdl_to_api_schema(schema_state(&supergraph_breaking_newer_rules())).unwrap();

        assert!(logs_contain(
            "breaks GraphQL September 2025 validation rules"
        ));
    }

    #[test]
    fn sdl_to_api_schema_rejects_plain_sdl_that_breaks_newer_rules() {
        let error = StateMachine::sdl_to_api_schema(schema_state(
            r#"type Query { me(id: ID! @deprecated(reason: "unused")): String }"#,
        ))
        .unwrap_err();

        assert!(
            matches!(error, ServerError::GraphQLSchema(_)),
            "expected a GraphQLSchema error, got {error}"
        );
    }

    #[test]
    fn sdl_to_api_schema_parses_plain_sdl() {
        let schema =
            StateMachine::sdl_to_api_schema(schema_state("type Query { id: String }")).unwrap();

        assert!(
            schema.type_field("Query", "id").is_ok(),
            "expected Query.id in the parsed schema"
        );
    }

    #[test]
    fn sdl_to_api_schema_returns_schema_error_for_invalid_sdl() {
        let error = StateMachine::sdl_to_api_schema(schema_state("type Query { id: Missing }"))
            .unwrap_err();

        assert!(
            matches!(error, ServerError::GraphQLSchema(_)),
            "expected a GraphQLSchema error, got {error}"
        );
    }
}
