//! Per-credential rate limiting applied before OAuth token validation.
//!
//! Each bearer credential gets its own token bucket, so one client that
//! replays an invalid token cannot make the server validate it more than a
//! configured number of times per second. The check runs before JWT parsing or
//! JWKS work. Requests that pass authentication get their token back, so only
//! rejected credentials use up the budget. Operators that already rate limit
//! upstream can turn the layer off with `transport.auth.rate_limit.enabled`.
//!
//! There is no per-peer limit. Behind a reverse proxy or load balancer the
//! connection address identifies the proxy, not the client, so keying on it
//! would make unrelated clients share one bucket.

use std::collections::{HashMap, hash_map::RandomState};
use std::hash::BuildHasher;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::generated::telemetry::TelemetryMetric;
use crate::meter;

use super::log_throttle::LogThrottle;

const DEFAULT_REQUESTS_PER_SECOND: NonZeroU32 = NonZeroU32::new(10).unwrap();
const DEFAULT_BURST: NonZeroU32 = NonZeroU32::new(50).unwrap();

/// Rate limiting applied to bearer credentials before token validation.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Set to `false` to turn the limiter off, for example when a gateway in
    /// front of the server already rate limits clients.
    pub enabled: bool,

    /// Sustained requests per second allowed for each bearer credential that
    /// fails authentication.
    pub requests_per_second: NonZeroU32,

    /// Failed authentications a credential can make in a burst before the
    /// sustained rate applies.
    pub burst: NonZeroU32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            requests_per_second: DEFAULT_REQUESTS_PER_SECOND,
            burst: DEFAULT_BURST,
        }
    }
}

/// Hard cap on tracked keys. When the map is full and a new key arrives,
/// fully-refilled buckets are evicted. If the map remains full, the new key
/// is allowed untracked so the limiter cannot become the denial of service.
const MAX_TRACKED_KEYS: usize = 50_000;

/// Minimum time between eviction sweeps. This prevents key churn from forcing
/// an O(n) scan on every request.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

struct Buckets {
    map: HashMap<u64, Bucket>,
    last_sweep: Option<Instant>,
}

/// In-memory token bucket rate limiter with a hard bound on tracked keys.
struct KeyedRateLimiter {
    rate: f64,
    burst: f64,
    max_tracked: usize,
    buckets: Mutex<Buckets>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LimitOutcome {
    Allowed,
    Rejected,
    AllowedUntracked,
}

impl KeyedRateLimiter {
    fn new(requests_per_second: u32, burst: u32, max_tracked: usize) -> Self {
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

    /// Consumes one token when the key is tracked and allowed. A new key is
    /// allowed without insertion when the bounded map has no space.
    fn check(&self, key: u64, now: Instant) -> LimitOutcome {
        let mut state = self.buckets.lock();

        if state.map.len() >= self.max_tracked && !state.map.contains_key(&key) {
            let sweep_due = state
                .last_sweep
                .is_none_or(|last| now.saturating_duration_since(last) >= SWEEP_INTERVAL);

            if sweep_due {
                let (rate, burst) = (self.rate, self.burst);
                state.map.retain(|_, bucket| {
                    refill(bucket, rate, burst, now);
                    bucket.tokens < burst
                });
                state.last_sweep = Some(now);
            }

            if state.map.len() >= self.max_tracked {
                return LimitOutcome::AllowedUntracked;
            }
        }

        let bucket = state.map.entry(key).or_insert(Bucket {
            tokens: self.burst,
            last_refill: now,
        });
        refill(bucket, self.rate, self.burst, now);

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            LimitOutcome::Allowed
        } else {
            LimitOutcome::Rejected
        }
    }

    /// Returns one token to a tracked key, up to the burst size.
    fn refund(&self, key: u64, now: Instant) {
        let mut state = self.buckets.lock();
        if let Some(bucket) = state.map.get_mut(&key) {
            refill(bucket, self.rate, self.burst, now);
            bucket.tokens = (bucket.tokens + 1.0).min(self.burst);
        }
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.buckets.lock().map.len()
    }
}

/// Adds tokens for the time since the last refill, capped at the burst size.
fn refill(bucket: &mut Bucket, rate: f64, burst: f64, now: Instant) {
    let elapsed = now
        .saturating_duration_since(bucket.last_refill)
        .as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rate).min(burst);
    bucket.last_refill = now;
}

/// Adds one to a rate-limit counter.
fn increment(metric: TelemetryMetric) {
    meter::METER
        .u64_counter(metric.as_str())
        .build()
        .add(1, &[]);
}

#[derive(Default)]
struct RateLimitReporter {
    rejected: LogThrottle,
    capacity_exhausted: LogThrottle,
}

impl RateLimitReporter {
    /// Counts a 429 and logs it, throttled.
    fn record_rejection(&self, now: Instant) {
        increment(TelemetryMetric::AuthRateLimitCount);
        if let Some(suppressed) = self.rejected.record(now) {
            tracing::warn!(suppressed, "request rate limited");
        }
    }

    /// Counts a request let through untracked because the limiter is full, and
    /// logs it, throttled.
    fn record_capacity_exhausted(&self, capacity: usize, now: Instant) {
        increment(TelemetryMetric::AuthRateLimitOverflowCount);
        if let Some(suppressed) = self.capacity_exhausted.record(now) {
            tracing::error!(
                capacity,
                suppressed,
                "rate limiter capacity exhausted; request allowed untracked"
            );
        }
    }
}

/// Shared state for the rate-limit layer.
#[derive(Clone)]
pub(crate) struct RateLimitState {
    credential_limiter: Arc<KeyedRateLimiter>,
    credential_hasher: RandomState,
    reporter: Arc<RateLimitReporter>,
}

impl RateLimitState {
    pub(crate) fn new(config: &RateLimitConfig) -> Self {
        Self::with_limiter(KeyedRateLimiter::new(
            config.requests_per_second.get(),
            config.burst.get(),
            MAX_TRACKED_KEYS,
        ))
    }

    fn with_limiter(limiter: KeyedRateLimiter) -> Self {
        Self {
            credential_limiter: Arc::new(limiter),
            credential_hasher: RandomState::new(),
            reporter: Arc::new(RateLimitReporter::default()),
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(requests_per_second: u32, burst: u32, max_tracked: usize) -> Self {
        Self::with_limiter(KeyedRateLimiter::new(
            requests_per_second,
            burst,
            max_tracked,
        ))
    }
}

/// Returns a keyed hash of the bearer credential, or `None` when the request
/// has no bearer credential.
fn credential_fingerprint(request: &Request, hasher: &RandomState) -> Option<u64> {
    let value = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }

    Some(hasher.hash_one(credential.trim_start()))
}

fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(http::header::RETRY_AFTER, "1")],
    )
        .into_response()
}

/// Enforces the bearer-credential limit. Requests without a bearer credential
/// pass through; the OAuth middleware decides them (401, or admitted by a
/// `skip_token_validation` rule) without any token validation. A request that
/// is not answered with 401 gets its token back.
pub(crate) async fn rate_limit(
    State(state): State<RateLimitState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(fingerprint) = credential_fingerprint(&request, &state.credential_hasher) else {
        return next.run(request).await;
    };

    let now = Instant::now();
    match state.credential_limiter.check(fingerprint, now) {
        LimitOutcome::Allowed => {}
        LimitOutcome::Rejected => {
            state.reporter.record_rejection(now);
            return too_many_requests();
        }
        LimitOutcome::AllowedUntracked => {
            state
                .reporter
                .record_capacity_exhausted(state.credential_limiter.max_tracked, now);
            return next.run(request).await;
        }
    }

    let response = next.run(request).await;
    if response.status() != StatusCode::UNAUTHORIZED {
        state.credential_limiter.refund(fingerprint, Instant::now());
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    mod config {
        use super::*;
        use rstest::rstest;

        #[test]
        fn defaults_are_on_at_ten_per_second_with_burst_of_fifty() {
            let config: RateLimitConfig = serde_yaml::from_str("{}").unwrap();
            assert!(config.enabled);
            assert_eq!(config.requests_per_second.get(), 10);
            assert_eq!(config.burst.get(), 50);
        }

        #[test]
        fn fields_can_be_set_individually() {
            let config: RateLimitConfig = serde_yaml::from_str("enabled: false\nburst: 5").unwrap();
            assert!(!config.enabled);
            assert_eq!(config.requests_per_second.get(), 10);
            assert_eq!(config.burst.get(), 5);
        }

        #[rstest]
        #[case::zero_rate("requests_per_second: 0")]
        #[case::zero_burst("burst: 0")]
        #[case::negative_rate("requests_per_second: -1")]
        #[case::unknown_field("max_tracked: 10")]
        fn rejects_invalid_values(#[case] yaml: &str) {
            assert!(serde_yaml::from_str::<RateLimitConfig>(yaml).is_err());
        }

        #[test]
        fn state_uses_configured_limits() {
            let config: RateLimitConfig =
                serde_yaml::from_str("requests_per_second: 3\nburst: 7").unwrap();
            let state = RateLimitState::new(&config);
            assert_eq!(state.credential_limiter.rate, 3.0);
            assert_eq!(state.credential_limiter.burst, 7.0);
            assert_eq!(state.credential_limiter.max_tracked, MAX_TRACKED_KEYS);
        }

        #[test]
        fn schema_exposes_every_field_with_a_minimum_of_one() {
            let schema = schemars::schema_for!(RateLimitConfig).to_value();
            let properties = &schema["properties"];
            assert!(properties.get("enabled").is_some());
            assert_eq!(properties["requests_per_second"]["minimum"], 1);
            assert_eq!(properties["burst"]["minimum"], 1);
        }
    }

    mod limiter {
        use super::{
            DEFAULT_BURST, DEFAULT_REQUESTS_PER_SECOND, KeyedRateLimiter, LimitOutcome,
            MAX_TRACKED_KEYS,
        };
        use rstest::rstest;
        use std::time::{Duration, Instant};

        fn allowed(limiter: &KeyedRateLimiter, key: u64, now: Instant) -> bool {
            limiter.check(key, now) != LimitOutcome::Rejected
        }

        /// Each step is `(key, milliseconds after start, expected allowed)`.
        #[rstest]
        #[case::allows_burst_then_blocks(1, 3, &[(1, 0, true), (1, 0, true), (1, 0, true), (1, 0, false)])]
        #[case::refills_over_time(2, 2, &[
            (1, 0, true), (1, 0, true), (1, 0, false),
            (1, 1_000, true), (1, 1_000, true), (1, 1_000, false),
        ])]
        #[case::refill_caps_at_burst(100, 2, &[
            (1, 0, true),
            (1, 3_600_000, true), (1, 3_600_000, true), (1, 3_600_000, false),
        ])]
        #[case::keys_are_limited_independently(1, 1, &[(1, 0, true), (1, 0, false), (2, 0, true)])]
        fn token_bucket(
            #[case] requests_per_second: u32,
            #[case] burst: u32,
            #[case] steps: &[(u64, u64, bool)],
        ) {
            let limiter = KeyedRateLimiter::new(requests_per_second, burst, MAX_TRACKED_KEYS);
            let start = Instant::now();
            for (step, &(key, millis, expected)) in steps.iter().enumerate() {
                let now = start + Duration::from_millis(millis);
                assert_eq!(allowed(&limiter, key, now), expected, "step {step}");
            }
        }

        #[test]
        fn credential_at_10x_is_clamped_and_other_credentials_are_unaffected() {
            let limiter = KeyedRateLimiter::new(
                DEFAULT_REQUESTS_PER_SECOND.get(),
                DEFAULT_BURST.get(),
                MAX_TRACKED_KEYS,
            );
            let start = Instant::now();
            let attacker = 66;

            let attacker_allowed = (0..1_000u32)
                .filter(|request| {
                    let now = start + Duration::from_millis(u64::from(*request) * 10);
                    allowed(&limiter, attacker, now)
                })
                .count();
            assert!(
                attacker_allowed <= 150,
                "attacker got {attacker_allowed} through"
            );

            for credential in 0..20 {
                for second in 0..10 {
                    let now = start + Duration::from_secs(second);
                    assert!(allowed(&limiter, credential, now));
                }
            }
        }

        #[test]
        fn full_map_keeps_active_buckets_and_does_not_grow() {
            let limiter = KeyedRateLimiter::new(1, 1, 2);
            let start = Instant::now();
            assert!(allowed(&limiter, 1, start));
            assert!(allowed(&limiter, 2, start));

            let now = start + Duration::from_millis(10);
            assert_eq!(
                limiter.check(3, now),
                LimitOutcome::AllowedUntracked,
                "new key should fail open"
            );
            assert_eq!(limiter.tracked_keys(), 2);
            assert!(!allowed(&limiter, 1, now), "active bucket was evicted");
        }

        #[test]
        fn sweep_is_throttled() {
            let limiter = KeyedRateLimiter::new(100, 1, 2);
            let start = Instant::now();
            assert!(allowed(&limiter, 1, start));
            assert!(allowed(&limiter, 2, start));

            let first_sweep = start + Duration::from_millis(5);
            assert!(allowed(&limiter, 3, first_sweep));
            assert_eq!(limiter.tracked_keys(), 2);

            let throttled = start + Duration::from_millis(20);
            assert!(allowed(&limiter, 4, throttled));
            assert_eq!(limiter.tracked_keys(), 2);

            let sweep_due = start + Duration::from_secs(2);
            assert!(allowed(&limiter, 5, sweep_due));
            assert_eq!(limiter.tracked_keys(), 1);
        }

        #[test]
        fn refund_returns_a_token_up_to_the_burst() {
            let limiter = KeyedRateLimiter::new(0, 2, MAX_TRACKED_KEYS);
            let now = Instant::now();
            assert!(allowed(&limiter, 1, now));
            assert!(allowed(&limiter, 1, now));
            assert!(!allowed(&limiter, 1, now));

            limiter.refund(1, now);
            assert!(allowed(&limiter, 1, now));
            assert!(!allowed(&limiter, 1, now));

            for _ in 0..5 {
                limiter.refund(1, now);
            }
            assert!(allowed(&limiter, 1, now));
            assert!(allowed(&limiter, 1, now));
            assert!(!allowed(&limiter, 1, now), "refund exceeded the burst");
        }

        #[test]
        fn refund_does_not_track_unknown_keys() {
            let limiter = KeyedRateLimiter::new(0, 1, MAX_TRACKED_KEYS);
            limiter.refund(1, Instant::now());
            assert_eq!(limiter.tracked_keys(), 0);
        }
    }

    mod middleware {
        use super::*;
        use axum::{
            Router,
            body::Body,
            http::{Request, StatusCode, header::AUTHORIZATION},
            middleware::from_fn_with_state,
            routing::get,
        };
        use tower::ServiceExt;
        use tracing_test::traced_test;

        /// Stands in for the OAuth middleware: `Bearer valid` passes
        /// authentication and every other request is answered with 401.
        fn app(requests_per_second: u32, burst: u32, max_tracked: usize) -> Router {
            let state = RateLimitState::for_test(requests_per_second, burst, max_tracked);
            Router::new()
                .route(
                    "/test",
                    get(|request: Request<Body>| async move {
                        match request.headers().get(AUTHORIZATION) {
                            Some(value) if value == "Bearer valid" => StatusCode::OK,
                            _ => StatusCode::UNAUTHORIZED,
                        }
                    }),
                )
                .layer(from_fn_with_state(state, rate_limit))
        }

        fn request(authorization: Option<&str>) -> Request<Body> {
            let mut request = Request::builder().uri("/test").body(Body::empty()).unwrap();
            if let Some(authorization) = authorization {
                request
                    .headers_mut()
                    .insert(AUTHORIZATION, authorization.parse().unwrap());
            }
            request
        }

        async fn status(app: &Router, authorization: Option<&str>) -> StatusCode {
            app.clone()
                .oneshot(request(authorization))
                .await
                .unwrap()
                .status()
        }

        #[tokio::test]
        async fn credential_limit_returns_429_with_retry_after() {
            let app = app(0, 1, MAX_TRACKED_KEYS);
            assert_eq!(
                status(&app, Some("Bearer token-a")).await,
                StatusCode::UNAUTHORIZED
            );

            let limited = app
                .clone()
                .oneshot(request(Some("Bearer token-a")))
                .await
                .unwrap();
            assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                limited.headers().get(http::header::RETRY_AFTER).unwrap(),
                "1"
            );
        }

        #[tokio::test]
        async fn each_credential_gets_its_own_bucket() {
            let app = app(0, 1, MAX_TRACKED_KEYS);
            assert_eq!(
                status(&app, Some("Bearer token-a")).await,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                status(&app, Some("Bearer token-a")).await,
                StatusCode::TOO_MANY_REQUESTS
            );

            // Known gap: with no per-peer limit, a client that invents a new
            // token per request is never limited here. Each token still costs
            // one bounded validation, and the kid-miss JWKS guard still applies.
            for token in ["Bearer token-b", "Bearer token-c"] {
                assert_eq!(
                    status(&app, Some(token)).await,
                    StatusCode::UNAUTHORIZED,
                    "{token}"
                );
            }
        }

        #[tokio::test]
        async fn bearer_scheme_is_case_insensitive_and_whitespace_tolerant() {
            let app = app(0, 1, MAX_TRACKED_KEYS);
            assert_eq!(
                status(&app, Some("Bearer token-a")).await,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                status(&app, Some("bearer  token-a")).await,
                StatusCode::TOO_MANY_REQUESTS
            );
        }

        #[tokio::test]
        async fn requests_without_bearer_credentials_pass_through() {
            let app = app(0, 1, MAX_TRACKED_KEYS);
            for authorization in [
                None,
                Some("Basic dXNlcjpwYXNz"),
                Some("token-without-scheme"),
            ] {
                for _ in 0..3 {
                    assert_eq!(
                        status(&app, authorization).await,
                        StatusCode::UNAUTHORIZED,
                        "{authorization:?}"
                    );
                }
            }
        }

        #[tokio::test]
        async fn authenticated_requests_do_not_use_up_the_budget() {
            let app = app(0, 1, MAX_TRACKED_KEYS);
            for _ in 0..5 {
                assert_eq!(status(&app, Some("Bearer valid")).await, StatusCode::OK);
            }

            assert_eq!(
                status(&app, Some("Bearer invalid")).await,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                status(&app, Some("Bearer invalid")).await,
                StatusCode::TOO_MANY_REQUESTS
            );
        }

        #[traced_test]
        #[tokio::test]
        async fn full_limiter_lets_new_credentials_through_untracked() {
            let app = app(0, 1, 1);
            assert_eq!(
                status(&app, Some("Bearer token-a")).await,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                status(&app, Some("Bearer token-a")).await,
                StatusCode::TOO_MANY_REQUESTS
            );

            // token-a's bucket is still active, so token-b is never tracked.
            for _ in 0..3 {
                assert_eq!(
                    status(&app, Some("Bearer token-b")).await,
                    StatusCode::UNAUTHORIZED
                );
            }
            assert!(logs_contain(
                "rate limiter capacity exhausted; request allowed untracked"
            ));
        }
    }
}
