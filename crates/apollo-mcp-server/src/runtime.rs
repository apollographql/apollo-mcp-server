//! Runtime utilites
//!
//! This module is only used by the main binary and provides helper code
//! related to runtime configuration.

mod config;
mod graphos;
mod introspection;
mod operation_source;
mod overrides;
mod schema_source;
mod schemas;

use std::path::Path;

pub use config::Config;
use figment::{
    Figment,
    providers::{Env, Format, Yaml},
};
pub use operation_source::{IdOrDefault, OperationSource};
pub use schema_source::SchemaSource;

/// Separator to use when drilling down into nested options in the env figment
const ENV_NESTED_SEPARATOR: &str = "__";

/// Read in a config from a YAML file, filling in any missing values from the environment
pub fn read_config(yaml_path: impl AsRef<Path>) -> Result<Config, figment::Error> {
    Figment::new()
        .join(apollo_common_env())
        .join(Env::prefixed("APOLLO_MCP_").split(ENV_NESTED_SEPARATOR))
        .join(Yaml::file(yaml_path))
        .extract()
}

/// Figment provider that handles mapping common Apollo environment variables into
/// the nested structure needed by the config
fn apollo_common_env() -> Env {
    Env::prefixed("APOLLO_")
        .only(&["graph_ref", "key", "uplink_endpoints"])
        .map(|key| match key.to_string().to_lowercase().as_str() {
            "graph_ref" => "GRAPHOS:APOLLO_GRAPH_REF".into(),
            "key" => "GRAPHOS:APOLLO_KEY".into(),
            "uplink_endpoints" => "GRAPHOS:APOLLO_UPLINK_ENDPOINTS".into(),

            // This case should never happen, so we just pass through this case as is
            other => other.to_string().into(),
        })
        .split(":")
}
