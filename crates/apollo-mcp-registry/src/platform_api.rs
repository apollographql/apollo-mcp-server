use secrecy::SecretString;
use std::fmt::Debug;
use std::time::Duration;

pub mod operation_collections;

/// Configuration for polling Apollo Uplink.
#[derive(Clone, Debug, Default)]
pub struct PlatformApiConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: SecretString,

    /// The duration between polling
    pub poll_interval: Duration,

    /// The HTTP client timeout for each poll
    pub timeout: Duration,
}
