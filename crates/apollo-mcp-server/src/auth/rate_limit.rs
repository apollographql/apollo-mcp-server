//! Per-client-IP rate limiting applied before OAuth token validation.
//!
//! This is the outermost layer of the auth stack: excess requests are
//! rejected with `429 Too Many Requests` before any JWT parsing or JWKS
//! work happens, bounding the CPU and network cost an unauthenticated
//! client can impose.

use ipnet::IpNet;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

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

/// Cap on distinct client IPs tracked at once. When reached, buckets that
/// are full (i.e. idle long enough to have completely refilled) are evicted.
/// At worst ~64 bytes per entry this bounds memory to a few megabytes.
#[allow(dead_code)] // consumed by the middleware (next commit)
const MAX_TRACKED_IPS: usize = 50_000;

#[allow(dead_code)] // fields accessed via refill function
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// In-memory token bucket rate limiter keyed by client IP.
#[allow(dead_code)] // consumed by the middleware (next commit)
pub(crate) struct IpRateLimiter {
    rate: f64,
    burst: f64,
    max_tracked: usize,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

#[allow(dead_code)] // consumed by the middleware (next commit)
impl IpRateLimiter {
    pub(crate) fn new(requests_per_second: u32, burst: u32) -> Self {
        Self::with_capacity(requests_per_second, burst, MAX_TRACKED_IPS)
    }

    fn with_capacity(requests_per_second: u32, burst: u32, max_tracked: usize) -> Self {
        Self {
            rate: f64::from(requests_per_second),
            burst: f64::from(burst),
            max_tracked,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a request from `ip` at time `now` is allowed. Consumes one
    /// token when allowed.
    pub(crate) fn check(&self, ip: IpAddr, now: Instant) -> bool {
        let mut buckets = self.buckets.lock();

        // Bound memory: when the map is full and this is a new IP, drop
        // buckets that have fully refilled (idle clients). Buckets below
        // capacity belong to recently active clients and are kept so an
        // ongoing attacker cannot reset their own bucket by flooding
        // fresh IPs.
        if buckets.len() >= self.max_tracked && !buckets.contains_key(&ip) {
            let (rate, burst) = (self.rate, self.burst);
            buckets.retain(|_, bucket| {
                refill(bucket, rate, burst, now);
                bucket.tokens < burst
            });
        }

        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.burst,
            last_refill: now,
        });
        refill(bucket, self.rate, self.burst, now);

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn tracked_ips(&self) -> usize {
        self.buckets.lock().len()
    }
}

#[allow(dead_code)] // consumed by the middleware (next commit)
fn refill(bucket: &mut Bucket, rate: f64, burst: f64, now: Instant) {
    let elapsed = now
        .saturating_duration_since(bucket.last_refill)
        .as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rate).min(burst);
    bucket.last_refill = now;
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

    mod limiter {
        use super::IpRateLimiter;
        use std::net::{IpAddr, Ipv4Addr};
        use std::time::{Duration, Instant};

        fn ip(last: u8) -> IpAddr {
            IpAddr::V4(Ipv4Addr::new(1, 2, 3, last))
        }

        #[test]
        fn allows_burst_then_blocks() {
            let limiter = IpRateLimiter::new(1, 3);
            let now = Instant::now();
            for i in 0..3 {
                assert!(limiter.check(ip(1), now), "request {i} within burst");
            }
            assert!(!limiter.check(ip(1), now), "request beyond burst");
        }

        #[test]
        fn refills_over_time() {
            let limiter = IpRateLimiter::new(2, 2); // 2 tokens/sec, burst 2
            let start = Instant::now();
            assert!(limiter.check(ip(1), start));
            assert!(limiter.check(ip(1), start));
            assert!(!limiter.check(ip(1), start));
            // 1 second later: 2 tokens refilled
            let later = start + Duration::from_secs(1);
            assert!(limiter.check(ip(1), later));
            assert!(limiter.check(ip(1), later));
            assert!(!limiter.check(ip(1), later));
        }

        #[test]
        fn refill_caps_at_burst() {
            let limiter = IpRateLimiter::new(100, 2);
            let start = Instant::now();
            assert!(limiter.check(ip(1), start)); // create the bucket
            // A long idle period must not accumulate more than `burst` tokens.
            let later = start + Duration::from_secs(3600);
            assert!(limiter.check(ip(1), later));
            assert!(limiter.check(ip(1), later));
            assert!(!limiter.check(ip(1), later));
        }

        #[test]
        fn ips_are_limited_independently() {
            let limiter = IpRateLimiter::new(1, 1);
            let now = Instant::now();
            assert!(limiter.check(ip(1), now));
            assert!(!limiter.check(ip(1), now));
            assert!(limiter.check(ip(2), now), "other IP must be unaffected");
        }

        #[test]
        fn evicts_idle_buckets_when_full() {
            let limiter = IpRateLimiter::with_capacity(1, 1, 2);
            let start = Instant::now();
            // Fill the map with two IPs, both spend their token (not idle).
            assert!(limiter.check(ip(1), start));
            assert!(limiter.check(ip(2), start));
            assert_eq!(limiter.tracked_ips(), 2);
            // Much later, both buckets are full again (idle) and a new IP
            // arrives: idle buckets get evicted, the new IP is admitted.
            let later = start + Duration::from_secs(60);
            assert!(limiter.check(ip(3), later));
            assert!(limiter.tracked_ips() <= 2, "idle buckets were evicted");
        }

        #[test]
        fn active_attacker_bucket_survives_eviction() {
            let limiter = IpRateLimiter::with_capacity(1, 1, 2);
            let start = Instant::now();
            assert!(limiter.check(ip(1), start)); // attacker spends its token
            assert!(limiter.check(ip(2), start));
            // Immediately after (no refill yet), a third IP triggers eviction.
            // The attacker's empty bucket must NOT be evicted, so the
            // attacker stays blocked.
            let now = start + Duration::from_millis(10);
            assert!(limiter.check(ip(3), now));
            assert!(!limiter.check(ip(1), now), "attacker still limited");
        }
    }
}
