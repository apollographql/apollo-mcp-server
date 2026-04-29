use rmcp::transport::StreamableHttpServerConfig;
use schemars::JsonSchema;
use serde::Deserialize;

/// Configuration for Host header validation to prevent DNS rebinding attacks.
///
/// Host validation is enforced by rmcp's Streamable HTTP transport via
/// [`StreamableHttpServerConfig::allowed_hosts`]. This struct is a thin,
/// stable surface for our YAML schema; [`HostValidationConfig::apply_to`]
/// translates it onto the rmcp config.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HostValidationConfig {
    /// Enable Host header validation (enabled by default for security).
    pub enabled: bool,

    /// Additional allowed hosts beyond the loopback defaults
    /// (`localhost`, `127.0.0.1`, `::1`). Entries may be bare hostnames
    /// (any port allowed) or `host:port` authorities.
    pub allowed_hosts: Vec<String>,
}

impl Default for HostValidationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_hosts: Vec::new(),
        }
    }
}

impl HostValidationConfig {
    /// Creates a configuration with Host header validation disabled.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            allowed_hosts: Vec::new(),
        }
    }

    /// Apply this host-validation configuration onto an rmcp
    /// [`StreamableHttpServerConfig`] and return the updated config.
    ///
    /// - `enabled = false` -> `disable_allowed_hosts()` (allow any host; not recommended)
    /// - `enabled = true, allowed_hosts = []` -> leave rmcp's defaults in place
    /// - `enabled = true, allowed_hosts = [..]` -> rmcp's defaults + user-supplied hosts
    ///
    /// rmcp's `with_allowed_hosts` replaces the list rather than appending, so
    /// when the user supplies extra hosts we read rmcp's existing defaults off
    /// of `rmcp_config` and merge them back in to preserve loopback access.
    #[must_use]
    pub(crate) fn apply_to(
        &self,
        rmcp_config: StreamableHttpServerConfig,
    ) -> StreamableHttpServerConfig {
        if !self.enabled {
            return rmcp_config.disable_allowed_hosts();
        }
        if self.allowed_hosts.is_empty() {
            return rmcp_config;
        }
        let mut merged = rmcp_config.allowed_hosts.clone();
        for host in &self.allowed_hosts {
            if !merged.iter().any(|h| h.eq_ignore_ascii_case(host)) {
                merged.push(host.clone());
            }
        }
        rmcp_config.with_allowed_hosts(merged)
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
        };
        let hosts = rmcp_allowed_hosts_after_apply(&config);
        let mut expected = StreamableHttpServerConfig::default().allowed_hosts;
        expected.push("mcp.test.com".to_string());
        assert_eq!(hosts, expected);
    }
}
