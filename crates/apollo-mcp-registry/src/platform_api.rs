use secrecy::SecretString;
use std::fmt::Debug;
use std::time::Duration;
use url::Url;

pub mod operation_collections;

const DEFAULT_PLATFORM_API: &str = "https://registry.apollographql.com/api/graphql";

/// Configuration for polling Apollo Uplink.
#[derive(Clone, Debug)]
pub struct PlatformApiConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: SecretString,

    /// The duration between polling
    pub poll_interval: Duration,

    /// The HTTP client timeout for each poll
    pub timeout: Duration,

    /// The URL of the Apollo registry
    pub registry_url: Url,
}

impl PlatformApiConfig {
    /// Creates a new `PlatformApiConfig` with the given Apollo key and default values for other fields.
    pub fn new(
        apollo_key: SecretString,
        poll_interval: Duration,
        timeout: Duration,
        registry_url: Option<Url>,
    ) -> Self {
        Self {
            apollo_key,
            poll_interval,
            timeout,
            #[allow(clippy::expect_used)]
            registry_url: registry_url
                .unwrap_or(Url::parse(DEFAULT_PLATFORM_API).expect("default URL should be valid")),
        }
    }
}
