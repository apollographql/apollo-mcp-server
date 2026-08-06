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
use std::time::{Duration, Instant};

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

/// Hard cap on distinct client IPs tracked at once (~100 bytes per entry
/// including HashMap overhead bounds memory to ~5 MB). When the map is
/// full and a new IP arrives, a sweep evicts fully-refilled (idle) buckets
/// — but only if the last sweep was more than [`SWEEP_INTERVAL`] ago.
/// If after the (possibly skipped) sweep the map is still full, the new IP
/// is allowed untracked (fail-open): a full map means an active flood, and
/// refusing every brand-new legitimate IP would turn the limiter into the
/// DoS. A brand-new IP's first request would be allowed anyway (fresh
/// bucket = full burst).
#[allow(dead_code)] // consumed by the middleware (next commit)
const MAX_TRACKED_IPS: usize = 50_000;

/// Minimum time between eviction sweeps. Prevents an attacker from forcing
/// an O(n) retain on every request while the map is full.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

#[allow(dead_code)] // fields accessed via refill function
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// Locked state: the bucket map plus a throttle timestamp for the eviction
/// sweep.
struct Buckets {
    map: HashMap<IpAddr, Bucket>,
    last_sweep: Option<Instant>,
}

/// In-memory token bucket rate limiter keyed by client IP.
#[allow(dead_code)] // consumed by the middleware (next commit)
pub(crate) struct IpRateLimiter {
    rate: f64,
    burst: f64,
    max_tracked: usize,
    buckets: Mutex<Buckets>,
}

#[allow(dead_code)] // consumed by the middleware (next commit)
impl IpRateLimiter {
    pub(crate) fn new(requests_per_second: u32, burst: u32) -> Self {
        Self::with_max_tracked(requests_per_second, burst, MAX_TRACKED_IPS)
    }

    fn with_max_tracked(requests_per_second: u32, burst: u32, max_tracked: usize) -> Self {
        Self {
            rate: f64::from(requests_per_second),
            burst: f64::from(burst),
            max_tracked,
            buckets: Mutex::new(Buckets {
                map: HashMap::new(),
                last_sweep: None,
            }),
        }
    }

    /// Whether a request from `ip` at time `now` is allowed. Consumes one
    /// token when allowed.
    pub(crate) fn check(&self, ip: IpAddr, now: Instant) -> bool {
        let mut state = self.buckets.lock();

        // Hard memory bound: when the map is full and this is a new IP,
        // attempt to evict idle buckets — but throttle the sweep to at most
        // once per SWEEP_INTERVAL so an attacker cannot force O(n) work on
        // every request. If after the (possibly skipped) sweep the map is
        // still full, allow the request untracked (fail-open). See the
        // MAX_TRACKED_IPS doc comment for the rationale.
        if state.map.len() >= self.max_tracked && !state.map.contains_key(&ip) {
            let sweep_due = state
                .last_sweep
                .is_none_or(|t| now.saturating_duration_since(t) >= SWEEP_INTERVAL);

            if sweep_due {
                let (rate, burst) = (self.rate, self.burst);
                state.map.retain(|_, bucket| {
                    refill(bucket, rate, burst, now);
                    bucket.tokens < burst
                });
                state.last_sweep = Some(now);
            }

            // Hard bound: if still full after the sweep (or sweep was
            // skipped), do not insert — allow untracked.
            if state.map.len() >= self.max_tracked {
                return true;
            }
        }

        let bucket = state.map.entry(ip).or_insert(Bucket {
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
        self.buckets.lock().map.len()
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
            // with_max_tracked: rate=1 token/s, burst=1, max=2
            let limiter = IpRateLimiter::with_max_tracked(1, 1, 2);
            let start = Instant::now();
            // Fill the map with two IPs, both spend their token (not idle).
            assert!(limiter.check(ip(1), start));
            assert!(limiter.check(ip(2), start));
            assert_eq!(limiter.tracked_ips(), 2);
            // Much later, both buckets are full again (idle) and a new IP
            // arrives: first sweep (last_sweep is None) evicts both idle
            // buckets; ip3 is inserted.
            let later = start + Duration::from_secs(60);
            assert!(limiter.check(ip(3), later));
            assert_eq!(
                limiter.tracked_ips(),
                1,
                "both idle buckets evicted, ip3 inserted"
            );
        }

        #[test]
        fn active_attacker_bucket_survives_eviction() {
            let limiter = IpRateLimiter::with_max_tracked(1, 1, 2);
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

        #[test]
        fn full_map_of_active_buckets_does_not_grow() {
            // rate=1 token/s, burst=2, max_tracked=2: fill the map with two
            // active (fully spent) IPs so the sweep has nothing to evict.
            let limiter = IpRateLimiter::with_max_tracked(1, 2, 2);
            let start = Instant::now();
            // Both IPs spend all tokens (active, not idle).
            assert!(limiter.check(ip(1), start));
            assert!(limiter.check(ip(1), start));
            assert!(limiter.check(ip(2), start));
            assert!(limiter.check(ip(2), start));
            assert_eq!(limiter.tracked_ips(), 2);
            // At start + 10ms buckets have not fully refilled (burst=2, rate=1,
            // so need 2s to refill). Sweep runs (first sweep, last_sweep None),
            // but no idle buckets exist → ip3 is allowed untracked (fail-open)
            // and the map stays at 2.
            let t1 = start + Duration::from_millis(10);
            assert!(limiter.check(ip(3), t1), "fail-open when map full");
            assert_eq!(limiter.tracked_ips(), 2, "map must not grow beyond max");
        }

        #[test]
        fn sweep_is_throttled() {
            // rate=100 tokens/s, burst=1 → refills fully in 10ms; max=100
            // so we track ip1 and ip2 normally.
            let limiter = IpRateLimiter::with_max_tracked(100, 1, 2);
            let start = Instant::now();
            // ip1 and ip2 spend their tokens at start.
            assert!(limiter.check(ip(1), start));
            assert!(limiter.check(ip(2), start));
            assert_eq!(limiter.tracked_ips(), 2);

            // At start + 5ms: ip3 arrives → first sweep (last_sweep None) runs.
            // Buckets have not yet fully refilled (10ms needed at 100/s for
            // burst=1 means 0.5 tokens after 5ms — below burst), so nothing is
            // evicted. ip3 is allowed untracked (fail-open). Map stays at 2.
            let t1 = start + Duration::from_millis(5);
            assert!(limiter.check(ip(3), t1), "ip3 allowed fail-open");
            assert_eq!(limiter.tracked_ips(), 2, "nothing evicted at 5ms");

            // At start + 20ms: buckets HAVE fully refilled (20ms × 100/s = 2 >
            // burst=1). But the sweep was last run at t1 (5ms ago → 15ms ago
            // from t2 perspective) — 15ms < 1s SWEEP_INTERVAL → sweep is
            // throttled. ip4 allowed untracked; map still 2.
            let t2 = start + Duration::from_millis(20);
            assert!(limiter.check(ip(4), t2), "ip4 allowed fail-open");
            assert_eq!(limiter.tracked_ips(), 2, "sweep throttled, map unchanged");

            // At start + 2s: SWEEP_INTERVAL has elapsed since last sweep.
            // Buckets have fully refilled (idle). Sweep runs, both evicted.
            // ip5 is inserted normally.
            let t3 = start + Duration::from_secs(2);
            assert!(limiter.check(ip(5), t3), "ip5 allowed after sweep");
            assert_eq!(
                limiter.tracked_ips(),
                1,
                "idle buckets evicted, ip5 inserted"
            );
        }
    }
}
