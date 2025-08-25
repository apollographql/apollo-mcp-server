use crate::custom_scalar_map::CustomScalarMap;
use crate::health::HealthCheckConfig;
use crate::operations::MutationMode;
use bon::bon;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue};

/// Common configuration options for the server
pub struct ServerConfig {
    pub(crate) headers: HeaderMap,
    pub(crate) execute_enabled: bool,
    pub(crate) validate_enabled: bool,
    pub(crate) introspect_enabled: bool,
    pub(crate) search_enabled: bool,
    pub(crate) introspect_minify: bool,
    pub(crate) search_minify: bool,
    pub(crate) explorer_graph_ref: Option<String>,
    pub(crate) custom_scalar_map: Option<CustomScalarMap>,
    pub(crate) mutation_mode: MutationMode,
    pub(crate) disable_type_description: bool,
    pub(crate) disable_schema_description: bool,
    pub(crate) search_leaf_depth: usize,
    pub(crate) index_memory_bytes: usize,
    pub(crate) health_check: HealthCheckConfig,
}

#[bon]
impl ServerConfig {
    #[builder]
    pub fn new(
        headers: HeaderMap,
        execute_enabled: bool,
        validate_enabled: bool,
        introspect_enabled: bool,
        search_enabled: bool,
        introspect_minify: bool,
        search_minify: bool,
        explorer_graph_ref: Option<String>,
        #[builder(required)] custom_scalar_map: Option<CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
        search_leaf_depth: usize,
        index_memory_bytes: usize,
        health_check: HealthCheckConfig,
    ) -> Self {
        let headers = {
            let mut headers = headers.clone();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers
        };

        Self {
            headers,
            execute_enabled,
            validate_enabled,
            introspect_enabled,
            search_enabled,
            introspect_minify,
            search_minify,
            explorer_graph_ref,
            custom_scalar_map,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
            search_leaf_depth,
            index_memory_bytes,
            health_check,
        }
    }
}
