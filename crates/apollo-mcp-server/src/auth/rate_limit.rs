//! Per-client-IP rate limiting applied before OAuth token validation.
//!
//! This is the outermost layer of the auth stack: excess requests are
//! rejected with `429 Too Many Requests` before any JWT parsing or JWKS
//! work happens, bounding the CPU and network cost an unauthenticated
//! client can impose.

use ipnet::IpNet;
use schemars::JsonSchema;
use serde::Deserialize;

/// Sustained requests per second allowed per client IP; not
/// operator-configurable (like the JWKS cache TTL, a knob can be added
/// later without a breaking change if real deployments need it).
#[allow(dead_code)]
pub(crate) const RATE_LIMIT_REQUESTS_PER_SECOND: u32 = 10;

/// Extra requests a client may send in a short spike before being
/// limited (the token bucket capacity); not operator-configurable.
#[allow(dead_code)]
pub(crate) const RATE_LIMIT_BURST: u32 = 50;

/// Per-client-IP rate limit configuration.
///
/// When this block is present, requests are counted per client IP using
/// a token bucket ([`RATE_LIMIT_BURST`] tokens, refilling at
/// [`RATE_LIMIT_REQUESTS_PER_SECOND`]). Requests that find the bucket
/// empty receive `429 Too Many Requests` before token validation runs.
#[allow(dead_code)] // consumed in the rate limiter middleware (next task)
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    /// IPs or CIDR ranges of trusted reverse proxies.
    ///
    /// When the connecting peer is in this list, the client IP is taken
    /// from the right-most `X-Forwarded-For` entry that is not itself a
    /// trusted proxy. When empty (default), the socket peer address is
    /// always used, which is the safe choice when no proxy sits in front
    /// of the server.
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub trusted_proxies: Vec<IpNet>,
}

#[cfg(test)]
mod tests {
    use super::*;

    mod config {
        use super::*;

        #[test]
        fn yaml_empty_block_enables_with_no_trusted_proxies() {
            let config: RateLimitConfig = serde_yaml::from_str("{}").unwrap();
            assert!(config.trusted_proxies.is_empty());
        }

        #[test]
        fn yaml_with_trusted_proxies() {
            let yaml = r#"
                trusted_proxies:
                  - 10.0.0.0/8
                  - 192.168.1.1/32
            "#;
            let config: RateLimitConfig = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(config.trusted_proxies.len(), 2);
        }

        #[test]
        fn yaml_rejects_unknown_fields() {
            let err = serde_yaml::from_str::<RateLimitConfig>("bogus_field: 1").unwrap_err();
            assert!(err.to_string().contains("bogus_field"), "got: {err}");
        }

        #[test]
        fn yaml_rejects_rate_knobs_that_are_not_exposed() {
            // Rate and burst are deliberately constants, not config. If a
            // future change exposes them, it must be a conscious decision —
            // this test forces that conversation.
            let err =
                serde_yaml::from_str::<RateLimitConfig>("requests_per_second: 5").unwrap_err();
            assert!(
                err.to_string().contains("requests_per_second"),
                "got: {err}"
            );
        }

        #[test]
        fn yaml_rejects_invalid_cidr() {
            let err = serde_yaml::from_str::<RateLimitConfig>("trusted_proxies: [\"not-a-cidr\"]")
                .unwrap_err();
            assert!(
                err.to_string().contains("invalid IP address syntax"),
                "got: {err}"
            );
        }
    }
}
