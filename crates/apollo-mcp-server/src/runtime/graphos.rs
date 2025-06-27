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
const APOLLO_UPLINK_ENDPOINTS_ENV: &str = "APOLLO_UPLINK_ENDPOINTS";

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
            .or(std::env::var(APOLLO_GRAPH_REF_ENV).ok())
            .ok_or_else(|| ServerError::EnvironmentVariable(APOLLO_GRAPH_REF_ENV.to_string()))
    }

    /// Extract the apollo key from the config or from the current env
    fn key(&self) -> Result<SecretString, ServerError> {
        self.apollo_key
            .clone()
            .or(std::env::var(APOLLO_KEY_ENV).map(Into::into).ok())
            .ok_or_else(|| ServerError::EnvironmentVariable(APOLLO_GRAPH_REF_ENV.to_string()))
    }

    /// Extract the apollo uplink endpoints from the config or from the current env
    fn uplink_endpoints(&self) -> Result<Vec<Url>, ServerError> {
        if !self.apollo_uplink_endpoints.is_empty() {
            Ok(self.apollo_uplink_endpoints.clone())
        } else if let Ok(csv) = std::env::var(APOLLO_UPLINK_ENDPOINTS_ENV) {
            parse_endpoints(&csv)
        } else {
            Ok(Vec::new())
        }
    }

    /// Generate an uplink config based on configuration params
    pub fn uplink_config(&self) -> Result<UplinkConfig, ServerError> {
        let uplink_endpoints = self.uplink_endpoints()?;
        let config = UplinkConfig {
            apollo_key: self.key()?,

            apollo_graph_ref: self.graph_ref()?,
            endpoints: uplink_endpoints
                .is_empty()
                .not()
                .then_some(Endpoints::Fallback {
                    urls: uplink_endpoints,
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

fn parse_endpoints(csv: &str) -> Result<Vec<Url>, ServerError> {
    csv.split(',')
        .map(|endpoint| Url::parse(endpoint.trim()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(ServerError::UrlParseError)
}

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        ffi::OsString,
        sync::{LazyLock, Mutex},
    };

    use url::Url;

    use crate::runtime::graphos::{APOLLO_KEY_ENV, APOLLO_UPLINK_ENDPOINTS_ENV};

    use super::{APOLLO_GRAPH_REF_ENV, GraphOSConfig, parse_endpoints};

    /// Guard for concurrency to ensure that tests don't stomp on each other's env
    static ENV_INJECTOR_GUARD: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    /// Env testing can be super unsafe, so we try to wrap it safely
    struct EnvInjector {
        variables: HashMap<String, Option<OsString>>,
    }

    impl EnvInjector {
        fn try_from_iter(iter: &[(&str, &str)]) -> Self {
            let _guard = ENV_INJECTOR_GUARD.lock().unwrap();

            let mut to_restore = HashMap::with_capacity(iter.len());
            for (k, v) in iter {
                // Save the old value
                to_restore.insert(k.to_string(), std::env::var_os(k));

                // SAFETY: This is safe because we are setting an env variable only for the context
                // of this test to ensure that it works as expected. The static mutex
                // also ensures that there won't be concurrent access to the env vars
                // during these tests. Care should be taken if env vars are modified
                // in other tests.
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var(k, v);
                }
            }

            Self {
                variables: to_restore,
            }
        }
    }

    impl Drop for EnvInjector {
        fn drop(&mut self) {
            let _guard = ENV_INJECTOR_GUARD.lock().unwrap();

            for (k, v) in &self.variables {
                // SAFETY: This is safe because we are setting an env variable only for the context
                // of this test to ensure that it works as expected. The static mutex
                // also ensures that there won't be concurrent access to the env vars
                // during these tests. Care should be taken if env vars are modified
                // in other tests.
                #[allow(unsafe_code)]
                unsafe {
                    if let Some(old) = v {
                        std::env::set_var(k, old);
                    } else {
                        std::env::remove_var(k);
                    }
                }
            }
        }
    }

    #[test]
    fn it_reads_from_env() {
        use secrecy::ExposeSecret;

        let graph_ref = "something@test123";
        let key = "abcxyz123";
        let _env =
            EnvInjector::try_from_iter(&[(APOLLO_GRAPH_REF_ENV, graph_ref), (APOLLO_KEY_ENV, key)]);

        let config = GraphOSConfig::default();
        assert_eq!(config.graph_ref().unwrap(), graph_ref.to_string());
        assert_eq!(config.key().unwrap().expose_secret(), key);
    }

    #[test]
    fn it_parses_uplink_endpoints_from_env() {
        let endpoints = [
            "https://example.com",
            "https://sub.example.com",
            "http://abc.xyz",
        ]
        .into_iter()
        .map(Url::parse)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        let as_str = endpoints
            .iter()
            .map(Url::as_str)
            .collect::<Vec<_>>()
            .join(",");

        let _env = EnvInjector::try_from_iter(&[(APOLLO_UPLINK_ENDPOINTS_ENV, &as_str)]);
        let config = GraphOSConfig::default();
        assert_eq!(config.uplink_endpoints().unwrap(), endpoints);
    }

    #[test]
    fn it_parses_uplink_endpoints() {
        let endpoints = [
            "https://example.com",
            "https://sub.example.com",
            "http://abc.xyz",
        ]
        .into_iter()
        .map(Url::parse)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

        let parsed = parse_endpoints(
            endpoints
                .iter()
                .map(Url::as_str)
                .collect::<Vec<_>>()
                .join(",")
                .as_str(),
        )
        .unwrap();

        assert_eq!(parsed, endpoints);
    }
}
