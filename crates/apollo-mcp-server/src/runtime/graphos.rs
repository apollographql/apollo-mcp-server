use std::{ops::Not as _, time::Duration};

use apollo_mcp_registry::{
    platform_api::PlatformApiConfig,
    uplink::{Endpoints, SecretString, UplinkConfig},
};
use apollo_mcp_server::errors::ServerError;
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

/// Credentials to use with GraphOS
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(default)]
pub struct GraphOSConfig {
    /// The apollo key
    #[schemars(with = "Option<String>")]
    pub apollo_key: Option<SecretString>,

    /// The graph reference
    pub apollo_graph_ref: Option<String>,

    /// The URL to use for Apollo's registry
    pub apollo_registry_url: Option<Url>,

    /// List of uplink URL overrides
    pub uplink_endpoints: Vec<Url>,
}

impl GraphOSConfig {
    /// Generate an uplink config based on configuration params
    pub fn uplink_config(&self) -> Result<UplinkConfig, ServerError> {
        let config = UplinkConfig {
            apollo_key: self
                .apollo_key
                .clone()
                .ok_or(ServerError::EnvironmentVariable(String::from("APOLLO_KEY")))?,

            apollo_graph_ref: self.apollo_graph_ref.clone().ok_or(
                ServerError::EnvironmentVariable(String::from("APOLLO_GRAPH_REF")),
            )?,
            endpoints: self
                .uplink_endpoints
                .is_empty()
                .not()
                .then_some(Endpoints::Fallback {
                    urls: self.uplink_endpoints.clone(),
                }),
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
                .ok_or(ServerError::EnvironmentVariable(String::from("APOLLO_KEY")))?,
            Duration::from_secs(30),
            Duration::from_secs(30),
            self.apollo_registry_url.clone(),
        );

        Ok(config)
    }
}
