use std::{ops::Not as _, time::Duration};

use apollo_mcp_registry::{
    platform_api::PlatformApiConfig,
    uplink::{Endpoints, SecretString, UplinkConfig},
};
use apollo_mcp_server::errors::ServerError;
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

const APOLLO_GRAPH_REF_ENV: &str = "APOLLO_GRAPH_REF";
const APOLLO_KEY_ENV: &str = "APOLLO_KEY";

/// Credentials to use with GraphOS
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct GraphOSConfig {
    /// The apollo key
    #[schemars(with = "Option<String>")]
    apollo_key: Option<SecretString>,

    /// The graph reference
    apollo_graph_ref: Option<String>,

    /// The URL to use for Apollo's registry
    apollo_registry_url: Option<Url>,

    /// List of uplink URL overrides
    apollo_uplink_endpoints: Vec<Url>,
}

impl GraphOSConfig {
    /// Extract the apollo graph reference from the config or from the current env
    pub fn graph_ref(&self) -> Result<String, ServerError> {
        self.apollo_graph_ref
            .clone()
            .ok_or_else(|| ServerError::EnvironmentVariable(APOLLO_GRAPH_REF_ENV.to_string()))
    }

    /// Extract the apollo key from the config or from the current env
    fn key(&self) -> Result<SecretString, ServerError> {
        self.apollo_key
            .clone()
            .ok_or_else(|| ServerError::EnvironmentVariable(APOLLO_GRAPH_REF_ENV.to_string()))
    }

    /// Generate an uplink config based on configuration params
    pub fn uplink_config(&self) -> Result<UplinkConfig, ServerError> {
        let config = UplinkConfig {
            apollo_key: self.key()?,

            apollo_graph_ref: self.graph_ref()?,
            endpoints: self.apollo_uplink_endpoints.is_empty().not().then_some(
                Endpoints::Fallback {
                    urls: self.apollo_uplink_endpoints.clone(),
                },
            ),
            poll_interval: Duration::from_secs(10),
            timeout: Duration::from_secs(30),
        };

        Ok(config)
    }

    /// Generate a platform API config based on configuration params
    pub fn platform_api_config(&self) -> Result<PlatformApiConfig, ServerError> {
        let config = PlatformApiConfig::new(
            self.apollo_key
                .clone()
                .ok_or(ServerError::EnvironmentVariable(APOLLO_KEY_ENV.to_string()))?,
            Duration::from_secs(30),
            Duration::from_secs(30),
            self.apollo_registry_url.clone(),
        );

        Ok(config)
    }
}
