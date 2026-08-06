//! Per-client-IP rate limiting applied before OAuth token validation.
//!
//! This is the outermost layer of the auth stack: excess requests are
//! rejected with `429 Too Many Requests` before any JWT parsing or JWKS
//! work happens, bounding the CPU and network cost an unauthenticated
//! client can impose.

use http::HeaderMap;
use ipnet::IpNet;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Sustained requests per second allowed per client IP; not
/// operator-configurable (like the JWKS cache TTL, a knob can be added
/// later without a breaking change if real deployments need it).
pub(crate) const RATE_LIMIT_REQUESTS_PER_SECOND: u32 = 10;

/// Extra requests a client may send in a short spike before being
/// limited (the token bucket capacity); not operator-configurable.
pub(crate) const RATE_LIMIT_BURST: u32 = 50;

/// Per-client-IP rate limit configuration.
///
/// When this block is present, requests are counted per client IP using
/// a token bucket ([`RATE_LIMIT_BURST`] tokens, refilling at
/// [`RATE_LIMIT_REQUESTS_PER_SECOND`]). Requests that find the bucket
/// empty receive `429 Too Many Requests` before token validation runs.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    /// CIDR ranges of trusted reverse proxies (use `/32` or `/128` for a
    /// single host, e.g. `10.0.0.1/32`).
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
const MAX_TRACKED_IPS: usize = 50_000;

/// Minimum time between eviction sweeps. Prevents an attacker from forcing
/// an O(n) retain on every request while the map is full.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

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
pub(crate) struct IpRateLimiter {
    rate: f64,
    burst: f64,
    max_tracked: usize,
    buckets: Mutex<Buckets>,
}

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

fn refill(bucket: &mut Bucket, rate: f64, burst: f64, now: Instant) {
    let elapsed = now
        .saturating_duration_since(bucket.last_refill)
        .as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rate).min(burst);
    bucket.last_refill = now;
}

/// Shared state for the rate-limit middleware.
#[derive(Clone)]
pub(crate) struct RateLimitState {
    limiter: Arc<IpRateLimiter>,
    trusted_proxies: Arc<[IpNet]>,
}

impl RateLimitState {
    pub(crate) fn new(config: &RateLimitConfig) -> Self {
        Self::with_limits(
            RATE_LIMIT_REQUESTS_PER_SECOND,
            RATE_LIMIT_BURST,
            config.trusted_proxies.clone(),
        )
    }

    /// Private constructor, doubling as the test seam: tests pass small
    /// limits, while production always goes through `new`, which applies
    /// the fixed constants.
    fn with_limits(requests_per_second: u32, burst: u32, trusted_proxies: Vec<IpNet>) -> Self {
        Self {
            limiter: Arc::new(IpRateLimiter::new(requests_per_second, burst)),
            trusted_proxies: Arc::from(trusted_proxies),
        }
    }
}

/// Axum middleware enforcing the per-IP rate limit. Runs before
/// `oauth_validate`, so limited requests never reach JWT parsing.
pub(crate) async fn rate_limit(
    State(state): State<RateLimitState>,
    request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0.ip());

    let Some(peer) = peer else {
        // Missing socket info means a server wiring regression. Rate
        // limiting is defence in depth, so fail open loudly rather than
        // rejecting all traffic.
        tracing::error!("rate limiter could not determine peer address; allowing request");
        return next.run(request).await;
    };

    let ip = client_ip(peer, request.headers(), &state.trusted_proxies);
    if state.limiter.check(ip, Instant::now()) {
        next.run(request).await
    } else {
        tracing::warn!(client_ip = %ip, "request rate limited");
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(http::header::RETRY_AFTER, "1")],
        )
            .into_response()
    }
}

const X_FORWARDED_FOR: &str = "x-forwarded-for";

fn is_trusted(ip: IpAddr, trusted_proxies: &[IpNet]) -> bool {
    trusted_proxies.iter().any(|net| net.contains(&ip))
}

/// The IP a request should be rate-limited under.
///
/// The socket peer address is authoritative unless it belongs to a
/// configured trusted proxy, in which case the right-most
/// `X-Forwarded-For` entry that is not itself a trusted proxy is the
/// client. A malformed entry or an all-trusted chain falls back to the
/// peer address, which shares one bucket across that proxy — conservative,
/// but never lets an attacker choose their own key.
///
/// IPv4-mapped IPv6 addresses (e.g. `::ffff:10.0.0.1`) are canonicalized
/// to their IPv4 form so that V4 CIDRs match dual-stack peers correctly.
fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted_proxies: &[IpNet]) -> IpAddr {
    let peer = peer.to_canonical();

    if trusted_proxies.is_empty() || !is_trusted(peer, trusted_proxies) {
        return peer;
    }

    // If any XFF header value is non-UTF8, fall back to peer — consistent
    // with how malformed IP entries are handled.
    let raw_values: Vec<&str> = match headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .map(|v| v.to_str())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(values) => values,
        Err(_) => return peer,
    };

    let entries: Vec<&str> = raw_values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();

    for entry in entries.iter().rev() {
        match entry.parse::<IpAddr>() {
            Ok(ip) => {
                let ip = ip.to_canonical();
                if is_trusted(ip, trusted_proxies) {
                    continue;
                }
                return ip;
            }
            Err(_) => break,
        }
    }
    peer
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

        #[test]
        fn yaml_rejects_bare_ip_without_prefix() {
            // Entries must be CIDR ranges; a bare IP needs /32 (or /128).
            let err = serde_yaml::from_str::<RateLimitConfig>("trusted_proxies: [\"10.0.0.1\"]")
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
        fn attacker_at_10x_is_clamped_and_legit_traffic_unaffected() {
            // The shipped constants: 10 req/s sustained, burst 50.
            let limiter = IpRateLimiter::new(
                crate::auth::rate_limit::RATE_LIMIT_REQUESTS_PER_SECOND,
                crate::auth::rate_limit::RATE_LIMIT_BURST,
            );
            let start = Instant::now();
            let attacker = ip(66);

            // Attacker sends 100 req/s for 10 seconds (10x the limit),
            // spread evenly (one request every 10ms).
            let mut allowed = 0u32;
            for i in 0..1000u32 {
                let now = start + Duration::from_millis(u64::from(i) * 10);
                if limiter.check(attacker, now) {
                    allowed += 1;
                }
            }
            // At most: initial burst (50) + 10/s refill over 10s (100).
            assert!(allowed <= 150, "attacker got {allowed} through");

            // Meanwhile 20 legitimate IPs each send 1 req/s: all allowed.
            for client in 0..20u8 {
                for second in 0..10u64 {
                    let now = start + Duration::from_secs(second);
                    assert!(
                        limiter.check(ip(client), now),
                        "legit client {client} blocked at second {second}"
                    );
                }
            }
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

    mod client_ip {
        use super::*;
        use http::HeaderMap;
        use rstest::rstest;
        use std::net::IpAddr;

        fn trusted(nets: &[&str]) -> Vec<IpNet> {
            nets.iter().map(|n| n.parse().unwrap()).collect()
        }

        fn xff(value: &str) -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", value.parse().unwrap());
            headers
        }

        #[rstest]
        // No trusted proxies: always the peer, XFF is attacker-controlled noise.
        #[case::no_proxies_ignores_xff(&[], "203.0.113.7", "9.9.9.9", "203.0.113.7")]
        // Peer is not a trusted proxy: XFF ignored.
        #[case::untrusted_peer_ignores_xff(&["10.0.0.0/8"], "203.0.113.7", "9.9.9.9", "203.0.113.7")]
        // Peer is trusted: right-most XFF entry is the client.
        #[case::trusted_peer_uses_xff(&["10.0.0.0/8"], "10.0.0.1", "198.51.100.4", "198.51.100.4")]
        // Right-most entry is another trusted proxy: skip to the real client.
        #[case::skips_trusted_hops(&["10.0.0.0/8"], "10.0.0.1", "198.51.100.4, 10.0.0.2", "198.51.100.4")]
        // Client-spoofed prefix is ignored; only the right-most untrusted entry counts.
        #[case::spoofed_prefix_ignored(&["10.0.0.0/8"], "10.0.0.1", "1.1.1.1, 198.51.100.4", "198.51.100.4")]
        fn resolves_client_ip(
            #[case] trusted_nets: &[&str],
            #[case] peer: &str,
            #[case] xff_value: &str,
            #[case] expected: &str,
        ) {
            let peer: IpAddr = peer.parse().unwrap();
            let expected: IpAddr = expected.parse().unwrap();
            let result = client_ip(peer, &xff(xff_value), &trusted(trusted_nets));
            assert_eq!(result, expected);
        }

        #[test]
        fn trusted_peer_without_xff_falls_back_to_peer() {
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &HeaderMap::new(), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, peer);
        }

        #[test]
        fn malformed_xff_entry_falls_back_to_peer() {
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &xff("not-an-ip"), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, peer);
        }

        #[test]
        fn all_trusted_chain_falls_back_to_peer() {
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &xff("10.0.0.2, 10.0.0.3"), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, peer);
        }

        #[test]
        fn multiple_xff_headers_treated_as_one_chain() {
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let mut headers = HeaderMap::new();
            headers.append("x-forwarded-for", "198.51.100.4".parse().unwrap());
            headers.append("x-forwarded-for", "10.0.0.2".parse().unwrap());
            let result = client_ip(peer, &headers, &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, "198.51.100.4".parse::<IpAddr>().unwrap());
        }

        #[test]
        fn ipv6_client_behind_trusted_proxy() {
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &xff("2001:db8::1"), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, "2001:db8::1".parse::<IpAddr>().unwrap());
        }

        #[test]
        fn ipv4_mapped_peer_matches_v4_trusted_cidr() {
            // A dual-stack listener delivers an IPv4 client as ::ffff:10.0.0.1.
            // The V4 CIDR 10.0.0.0/8 must still recognize it as trusted so the
            // XFF chain is consulted.
            let peer: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &xff("198.51.100.4"), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, "198.51.100.4".parse::<IpAddr>().unwrap());
        }

        #[test]
        fn ipv4_mapped_xff_entry_is_canonicalized() {
            // An XFF entry written as ::ffff:198.51.100.4 should resolve to the
            // canonical V4 form 198.51.100.4 so it keys the same bucket.
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &xff("::ffff:198.51.100.4"), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, "198.51.100.4".parse::<IpAddr>().unwrap());
        }

        #[test]
        fn empty_xff_value_falls_back_to_peer() {
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &xff(""), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, peer);
        }

        #[test]
        fn xff_entry_with_port_falls_back_to_peer() {
            // Port-stripping is a deliberate non-feature until a real deployment
            // needs it; entries like "1.2.3.4:5678" are treated as malformed.
            let peer: IpAddr = "10.0.0.1".parse().unwrap();
            let result = client_ip(peer, &xff("1.2.3.4:5678"), &trusted(&["10.0.0.0/8"]));
            assert_eq!(result, peer);
        }
    }

    mod middleware {
        use super::*;
        use axum::{
            Router,
            body::Body,
            extract::ConnectInfo,
            http::{Request, StatusCode},
            middleware::from_fn_with_state,
            routing::get,
        };
        use std::net::SocketAddr;
        use tower::ServiceExt;

        fn app(rps: u32, burst: u32) -> Router {
            // Tests use small limits via the internal constructor; production
            // always uses the RATE_LIMIT_* constants through `new`.
            let state = RateLimitState::with_limits(rps, burst, vec![]);
            Router::new()
                .route("/test", get(|| async { "ok" }))
                .layer(from_fn_with_state(state, rate_limit))
        }

        fn request_from(ip: &str) -> Request<Body> {
            let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();
            let addr: SocketAddr = format!("{ip}:12345").parse().unwrap();
            req.extensions_mut().insert(ConnectInfo(addr));
            req
        }

        #[tokio::test]
        async fn allows_within_burst() {
            let app = app(1, 2);
            for _ in 0..2 {
                let res = app
                    .clone()
                    .oneshot(request_from("203.0.113.7"))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }
        }

        #[tokio::test]
        async fn returns_429_with_retry_after_when_over_limit() {
            let app = app(1, 1);
            let ok = app
                .clone()
                .oneshot(request_from("203.0.113.7"))
                .await
                .unwrap();
            assert_eq!(ok.status(), StatusCode::OK);
            let limited = app
                .clone()
                .oneshot(request_from("203.0.113.7"))
                .await
                .unwrap();
            assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                limited.headers().get(http::header::RETRY_AFTER).unwrap(),
                "1"
            );
        }

        #[tokio::test]
        async fn other_ips_unaffected() {
            let app = app(1, 1);
            let _ = app
                .clone()
                .oneshot(request_from("203.0.113.7"))
                .await
                .unwrap();
            let limited = app
                .clone()
                .oneshot(request_from("203.0.113.7"))
                .await
                .unwrap();
            assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
            let other = app
                .clone()
                .oneshot(request_from("203.0.113.8"))
                .await
                .unwrap();
            assert_eq!(other.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn missing_connect_info_fails_open() {
            let app = app(1, 1);
            // No ConnectInfo extension: wiring regression. Requests pass.
            for _ in 0..3 {
                let req = Request::builder().uri("/test").body(Body::empty()).unwrap();
                let res = app.clone().oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::OK);
            }
        }
    }
}
