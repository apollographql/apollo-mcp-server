use rmcp::transport::StreamableHttpServerConfig;
use schemars::JsonSchema;
use serde::Deserialize;

/// Configuration for Host and Origin header validation to prevent DNS
/// rebinding and cross-origin attacks.
///
/// Both checks are enforced by rmcp's Streamable HTTP transport via
/// [`StreamableHttpServerConfig::allowed_hosts`] and
/// [`StreamableHttpServerConfig::allowed_origins`]. This struct is a thin,
/// stable surface for our YAML schema; [`HostValidationConfig::apply_to`]
/// translates it onto the rmcp config.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HostValidationConfig {
    /// Enable Host validation and any configured Origin validation (default: true).
    pub enabled: bool,

    /// Additional allowed hosts beyond the loopback defaults
    /// (`localhost`, `127.0.0.1`, `::1`). Entries may be bare hostnames
    /// (any port allowed) or `host:port` authorities.
    pub allowed_hosts: Vec<String>,

    /// Allowed browser origins for inbound `Origin` validation (RFC 6454).
    /// Empty (the default) leaves Origin validation disabled, matching rmcp.
    /// When non-empty, requests carrying an `Origin` header must match one of
    /// these `(scheme, host, port)` entries; entries must include a scheme
    /// (for example `https://app.example.com`), and `"null"` matches the
    /// browser's `Origin: null`.
    pub allowed_origins: Vec<String>,
}

impl Default for HostValidationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
        }
    }
}

impl HostValidationConfig {
    /// Creates a configuration with Host and Origin validation disabled.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
        }
    }

    /// Apply this validation configuration onto an rmcp
    /// [`StreamableHttpServerConfig`] and return the updated config.
    ///
    /// Host validation (DNS rebinding protection):
    /// - `enabled = false` -> `disable_allowed_hosts()` (allow any host; not recommended)
    /// - `enabled = true, allowed_hosts = []` -> leave rmcp's loopback defaults in place
    /// - `enabled = true, allowed_hosts = [..]` -> rmcp's defaults + user-supplied hosts
    ///
    /// Origin validation (RFC 6454), opt-in and disabled by default in rmcp:
    /// - `enabled = false` -> `disable_allowed_origins()`
    /// - `enabled = true, allowed_origins = []` -> leave Origin validation disabled
    /// - `enabled = true, allowed_origins = [..]` -> enforce the given origins
    ///
    /// rmcp's `with_allowed_hosts` replaces the list rather than appending, so
    /// when the user supplies extra hosts we read rmcp's existing defaults off
    /// of `rmcp_config` and merge them back in to preserve loopback access.
    /// Origins have no rmcp defaults, so the user list is set as-is.
    #[must_use]
    pub(crate) fn apply_to(
        &self,
        rmcp_config: StreamableHttpServerConfig,
    ) -> StreamableHttpServerConfig {
        if !self.enabled {
            return rmcp_config
                .disable_allowed_hosts()
                .disable_allowed_origins();
        }

        let mut config = rmcp_config;

        if !self.allowed_hosts.is_empty() {
            let mut merged = config.allowed_hosts.clone();
            for host in &self.allowed_hosts {
                if !merged.iter().any(|h| h.eq_ignore_ascii_case(host)) {
                    merged.push(host.clone());
                }
            }
            config = config.with_allowed_hosts(merged);
        }

        if !self.allowed_origins.is_empty() {
            config = config.with_allowed_origins(self.allowed_origins.clone());
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rmcp_allowed_hosts_after_apply(config: &HostValidationConfig) -> Vec<String> {
        config
            .apply_to(StreamableHttpServerConfig::default())
            .allowed_hosts
    }

    fn rmcp_allowed_origins_after_apply(config: &HostValidationConfig) -> Vec<String> {
        config
            .apply_to(StreamableHttpServerConfig::default())
            .allowed_origins
    }

    #[test]
    fn default_config_is_enabled() {
        let config = HostValidationConfig::default();
        assert!(config.enabled);
        assert!(config.allowed_hosts.is_empty());
    }

    #[test]
    fn disabled_constructor_returns_disabled_state() {
        let config = HostValidationConfig::disabled();
        assert!(!config.enabled);
        assert!(config.allowed_hosts.is_empty());
    }

    #[test]
    fn enabled_default_keeps_rmcp_defaults() {
        let hosts = rmcp_allowed_hosts_after_apply(&HostValidationConfig::default());
        assert_eq!(hosts, StreamableHttpServerConfig::default().allowed_hosts);
    }

    #[test]
    fn disabled_clears_allowed_hosts() {
        let hosts = rmcp_allowed_hosts_after_apply(&HostValidationConfig::disabled());
        assert!(hosts.is_empty());
    }

    #[test]
    fn user_hosts_are_merged_with_rmcp_defaults() {
        let config = HostValidationConfig {
            enabled: true,
            allowed_hosts: vec!["mcp.test.com".into(), "mcp.test.com:8080".into()],
            allowed_origins: Vec::new(),
        };
        let hosts = rmcp_allowed_hosts_after_apply(&config);
        let mut expected = StreamableHttpServerConfig::default().allowed_hosts;
        expected.extend(["mcp.test.com".to_string(), "mcp.test.com:8080".to_string()]);
        assert_eq!(hosts, expected);
    }

    #[test]
    fn user_hosts_are_deduplicated_against_defaults() {
        let config = HostValidationConfig {
            enabled: true,
            allowed_hosts: vec!["LOCALHOST".into(), "mcp.test.com".into()],
            allowed_origins: Vec::new(),
        };
        let hosts = rmcp_allowed_hosts_after_apply(&config);
        let mut expected = StreamableHttpServerConfig::default().allowed_hosts;
        expected.push("mcp.test.com".to_string());
        assert_eq!(hosts, expected);
    }

    #[test]
    fn enabled_default_has_no_allowed_origins() {
        let origins = rmcp_allowed_origins_after_apply(&HostValidationConfig::default());
        assert!(origins.is_empty());
    }

    #[test]
    fn user_origins_are_applied_when_enabled() {
        let config = HostValidationConfig {
            enabled: true,
            allowed_hosts: Vec::new(),
            allowed_origins: vec![
                "https://app.example.com".into(),
                "http://localhost:8080".into(),
            ],
        };
        let origins = rmcp_allowed_origins_after_apply(&config);
        assert_eq!(
            origins,
            vec![
                "https://app.example.com".to_string(),
                "http://localhost:8080".to_string()
            ]
        );
    }

    #[test]
    fn disabled_clears_allowed_origins() {
        let config = HostValidationConfig {
            enabled: false,
            allowed_hosts: Vec::new(),
            allowed_origins: vec!["https://app.example.com".into()],
        };
        let origins = rmcp_allowed_origins_after_apply(&config);
        assert!(origins.is_empty());
    }
}
