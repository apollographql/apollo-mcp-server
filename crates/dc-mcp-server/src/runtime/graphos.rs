use std::{ops::Not as _, time::Duration};

use apollo_mcp_registry::{
    platform_api::PlatformApiConfig,
    uplink::{Endpoints, SecretString, UplinkConfig},
};
use dc_mcp_server::errors::ServerError;
use schemars::JsonSchema;
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use url::Url;

#[cfg(test)]
use serde::Serialize;

const DC_API_KEY_ENV: &str = "DC_API_KEY";
const DEFAULT_GRAPH_REF: &str = "docontrol-api@current";

fn apollo_uplink_endpoints_deserializer<'de, D>(deserializer: D) -> Result<Vec<Url>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum UrlListOrString {
        List(Vec<Url>),
        String(String),
    }

    match UrlListOrString::deserialize(deserializer)? {
        UrlListOrString::List(urls) => Ok(urls),
        UrlListOrString::String(s) => s
            .split(',')
            .map(|v| {
                Url::parse(v.trim()).map_err(|e| {
                    D::Error::custom(format!("Could not parse uplink endpoint URL: {e}"))
                })
            })
            .collect(),
    }
}

/// Credentials to use with GraphOS
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[cfg_attr(test, derive(Serialize))]
#[serde(default)]
pub struct GraphOSConfig {
    /// The apollo key
    #[schemars(with = "Option<String>")]
    #[cfg_attr(test, serde(skip_serializing))]
    apollo_key: Option<SecretString>,

    /// The graph reference
    apollo_graph_ref: Option<String>,

    /// The URL to use for Apollo's registry
    apollo_registry_url: Option<Url>,

    /// List of uplink URL overrides
    #[serde(deserialize_with = "apollo_uplink_endpoints_deserializer")]
    apollo_uplink_endpoints: Vec<Url>,
}

impl GraphOSConfig {
    /// Extract the apollo graph reference from the config, env, or use hardcoded default
    #[allow(clippy::result_large_err)]
    pub fn graph_ref(&self) -> Result<String, ServerError> {
        Ok(self
            .apollo_graph_ref
            .clone()
            .unwrap_or_else(|| DEFAULT_GRAPH_REF.to_string()))
    }

    /// Extract the apollo key from the config or from the current env
    #[allow(clippy::result_large_err)]
    fn key(&self) -> Result<SecretString, ServerError> {
        self.apollo_key
            .clone()
            .ok_or_else(|| ServerError::EnvironmentVariable(DC_API_KEY_ENV.to_string()))
    }

    /// Generate an uplink config based on configuration params
    #[allow(clippy::result_large_err)]
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
    #[allow(clippy::result_large_err)]
    pub fn platform_api_config(&self) -> Result<PlatformApiConfig, ServerError> {
        let config = PlatformApiConfig::new(
            self.apollo_key
                .clone()
                .ok_or(ServerError::EnvironmentVariable(DC_API_KEY_ENV.to_string()))?,
            Duration::from_secs(30),
            Duration::from_secs(30),
            self.apollo_registry_url.clone(),
        );

        Ok(config)
    }
}
