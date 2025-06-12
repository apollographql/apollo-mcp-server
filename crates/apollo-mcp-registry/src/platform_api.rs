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

#[cfg(test)]
mod test {
    use super::*;
    use secrecy::{ExposeSecret, SecretString};
    use std::time::Duration;

    #[test]
    fn test_platform_api_config_with_none_endpoints() {
        let config = PlatformApiConfig::new(
            SecretString::from("test_apollo_key"),
            Duration::from_secs(10),
            Duration::from_secs(5),
            None,
        );
        assert_eq!(config.apollo_key.expose_secret(), "test_apollo_key");
        assert_eq!(config.poll_interval, Duration::from_secs(10));
        assert_eq!(config.timeout, Duration::from_secs(5));
        assert_eq!(config.registry_url.to_string(), DEFAULT_PLATFORM_API);
    }
}
