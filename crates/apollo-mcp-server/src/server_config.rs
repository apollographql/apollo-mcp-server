use crate::custom_scalar_map::CustomScalarMap;
use crate::operations::MutationMode;
use bon::bon;
use http::HeaderMap;

/// Common configuration options for the server
pub struct ServerConfig {
    pub(crate) headers: HeaderMap,
    pub(crate) execute_introspection: bool,
    pub(crate) validate_introspection: bool,
    pub(crate) introspect_introspection: bool,
    pub(crate) search_introspection: bool,
    pub(crate) introspect_minify: bool,
    pub(crate) search_minify: bool,
    pub(crate) explorer_graph_ref: Option<String>,
    pub(crate) custom_scalar_map: Option<CustomScalarMap>,
    pub(crate) mutation_mode: MutationMode,
    pub(crate) disable_type_description: bool,
    pub(crate) disable_schema_description: bool,
    pub(crate) search_leaf_depth: usize,
    pub(crate) index_memory_bytes: usize,
}

#[bon]
impl ServerConfig {
    #[builder]
    pub fn new(
        headers: HeaderMap,
        execute_introspection: bool,
        validate_introspection: bool,
        introspect_introspection: bool,
        search_introspection: bool,
        introspect_minify: bool,
        search_minify: bool,
        explorer_graph_ref: Option<String>,
        #[builder(required)] custom_scalar_map: Option<CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
        search_leaf_depth: usize,
        index_memory_bytes: usize,
    ) -> Self {
        Self {
            headers,
            execute_introspection,
            validate_introspection,
            introspect_introspection,
            search_introspection,
            introspect_minify,
            search_minify,
            explorer_graph_ref,
            custom_scalar_map,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
            search_leaf_depth,
            index_memory_bytes,
        }
    }
}
