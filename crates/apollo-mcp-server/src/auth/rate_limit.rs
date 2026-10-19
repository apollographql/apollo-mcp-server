//! Per-credential rate limiting applied before OAuth token validation.
//!
//! Each bearer credential gets its own token bucket, so one client that
//! replays a token cannot make the server validate it more than a fixed
//! number of times per second. The check runs before JWT parsing or JWKS work.
//!
//! There is no per-peer limit. Behind a reverse proxy or load balancer the
//! connection address identifies the proxy, not the client, so keying on it
//! would make unrelated clients share one bucket. A per-client limit that
//! understands trusted proxies is planned as a follow-up.

use std::collections::{HashMap, hash_map::RandomState};
use std::hash::{BuildHasher, Hash};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse, Response},
};
use opentelemetry::KeyValue;
use parking_lot::Mutex;

use crate::generated::telemetry::{TelemetryAttribute, TelemetryMetric};
use crate::meter;

use super::log_throttle::LogThrottle;

const CREDENTIAL_REQUESTS_PER_SECOND: u32 = 10;
const CREDENTIAL_BURST: u32 = 50;

/// Hard cap on tracked keys. When the map is full and a new key arrives,
/// fully-refilled buckets are evicted. If the map remains full, the new key
/// is allowed untracked so the limiter cannot become the denial of service.
const MAX_TRACKED_KEYS: usize = 50_000;

/// Minimum time between eviction sweeps. This prevents key churn from forcing
/// an O(n) scan on every request.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct Limits {
    requests_per_second: u32,
    burst: u32,
}

impl Limits {
    const fn new(requests_per_second: u32, burst: u32) -> Self {
        Self {
            requests_per_second,
            burst,
        }
    }
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

struct Buckets<K> {
    map: HashMap<K, Bucket>,
    last_sweep: Option<Instant>,
}

/// In-memory token bucket rate limiter with a hard bound on tracked keys.
struct KeyedRateLimiter<K> {
    rate: f64,
    burst: f64,
    max_tracked: usize,
    buckets: Mutex<Buckets<K>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LimitOutcome {
    Allowed,
    Rejected,
    AllowedUntracked,
}

impl<K> KeyedRateLimiter<K>
where
    K: Copy + Eq + Hash,
{
    fn new(limits: Limits) -> Self {
        Self::with_max_tracked(limits, MAX_TRACKED_KEYS)
    }

    fn with_max_tracked(limits: Limits, max_tracked: usize) -> Self {
        Self {
            rate: f64::from(limits.requests_per_second),
            burst: f64::from(limits.burst),
            max_tracked,
            buckets: Mutex::new(Buckets {
                map: HashMap::new(),
                last_sweep: None,
            }),
        }
    }

    /// Consumes one token when the key is tracked and allowed. A new key is
    /// allowed without insertion when the bounded map has no space.
    fn check(&self, key: K, now: Instant) -> LimitOutcome {
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

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
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

/// Which limiter produced an event. Recorded as the `rate_limit_kind`
/// metric attribute so a future per-peer limiter can share the counters.
#[derive(Clone, Copy)]
enum RejectionKind {
    Credential,
}

impl RejectionKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Credential => "credential",
        }
    }
}

#[derive(Default)]
struct RateLimitReporter {
    credential: LogThrottle,
    credential_capacity: LogThrottle,
}

impl RateLimitReporter {
    fn record_rejection(&self, kind: RejectionKind, now: Instant) {
        meter::METER
            .u64_counter(TelemetryMetric::AuthRateLimitCount.as_str())
            .build()
            .add(
                1,
                &[KeyValue::new(
                    TelemetryAttribute::RateLimitKind.to_key(),
                    kind.as_str(),
                )],
            );

        let window = match kind {
            RejectionKind::Credential => &self.credential,
        };
        if let Some(suppressed) = window.record(now) {
            tracing::warn!(
                rate_limit_kind = kind.as_str(),
                suppressed,
                "request rate limited"
            );
        }
    }

    fn record_capacity_exhausted(&self, kind: RejectionKind, now: Instant) {
        meter::METER
            .u64_counter(TelemetryMetric::AuthRateLimitOverflowCount.as_str())
            .build()
            .add(
                1,
                &[KeyValue::new(
                    TelemetryAttribute::RateLimitKind.to_key(),
                    kind.as_str(),
                )],
            );

        let window = match kind {
            RejectionKind::Credential => &self.credential_capacity,
        };
        if let Some(suppressed) = window.record(now) {
            tracing::error!(
                rate_limit_kind = kind.as_str(),
                capacity = MAX_TRACKED_KEYS,
                suppressed,
                "rate limiter capacity exhausted; request allowed untracked"
            );
        }
    }
}

/// Shared state for the rate-limit layer.
#[derive(Clone)]
pub(crate) struct RateLimitState {
    credential_limiter: Arc<KeyedRateLimiter<u64>>,
    credential_hasher: RandomState,
    reporter: Arc<RateLimitReporter>,
}

impl RateLimitState {
    pub(crate) fn new() -> Self {
        Self::with_limits(Limits::new(
            CREDENTIAL_REQUESTS_PER_SECOND,
            CREDENTIAL_BURST,
        ))
    }

    fn with_limits(credential: Limits) -> Self {
        Self {
            credential_limiter: Arc::new(KeyedRateLimiter::new(credential)),
            credential_hasher: RandomState::new(),
            reporter: Arc::new(RateLimitReporter::default()),
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(credential_requests_per_second: u32, credential_burst: u32) -> Self {
        Self::with_limits(Limits::new(
            credential_requests_per_second,
            credential_burst,
        ))
    }
}

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
/// `skip_token_validation` rule) without any token validation.
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
            state
                .reporter
                .record_rejection(RejectionKind::Credential, now);
            return too_many_requests();
        }
        LimitOutcome::AllowedUntracked => state
            .reporter
            .record_capacity_exhausted(RejectionKind::Credential, now),
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    mod limiter {
        use super::{
            CREDENTIAL_BURST, CREDENTIAL_REQUESTS_PER_SECOND, KeyedRateLimiter, LimitOutcome,
            Limits,
        };
        use std::time::{Duration, Instant};

        fn allowed<K>(limiter: &KeyedRateLimiter<K>, key: K, now: Instant) -> bool
        where
            K: Copy + Eq + std::hash::Hash,
        {
            limiter.check(key, now) != LimitOutcome::Rejected
        }

        #[test]
        fn allows_burst_then_blocks() {
            let limiter = KeyedRateLimiter::new(Limits::new(1, 3));
            let now = Instant::now();
            for request in 0..3 {
                assert!(allowed(&limiter, 1, now), "request {request} within burst");
            }
            assert!(!allowed(&limiter, 1, now), "request beyond burst");
        }

        #[test]
        fn refills_over_time() {
            let limiter = KeyedRateLimiter::new(Limits::new(2, 2));
            let start = Instant::now();
            assert!(allowed(&limiter, 1, start));
            assert!(allowed(&limiter, 1, start));
            assert!(!allowed(&limiter, 1, start));

            let later = start + Duration::from_secs(1);
            assert!(allowed(&limiter, 1, later));
            assert!(allowed(&limiter, 1, later));
            assert!(!allowed(&limiter, 1, later));
        }

        #[test]
        fn refill_caps_at_burst() {
            let limiter = KeyedRateLimiter::new(Limits::new(100, 2));
            let start = Instant::now();
            assert!(allowed(&limiter, 1, start));

            let later = start + Duration::from_secs(3_600);
            assert!(allowed(&limiter, 1, later));
            assert!(allowed(&limiter, 1, later));
            assert!(!allowed(&limiter, 1, later));
        }

        #[test]
        fn keys_are_limited_independently() {
            let limiter = KeyedRateLimiter::new(Limits::new(1, 1));
            let now = Instant::now();
            assert!(allowed(&limiter, 1, now));
            assert!(!allowed(&limiter, 1, now));
            assert!(allowed(&limiter, 2, now));
        }

        #[test]
        fn evicts_idle_buckets_when_full() {
            let limiter = KeyedRateLimiter::with_max_tracked(Limits::new(1, 1), 2);
            let start = Instant::now();
            assert!(allowed(&limiter, 1, start));
            assert!(allowed(&limiter, 2, start));
            assert_eq!(limiter.tracked_keys(), 2);

            let later = start + Duration::from_secs(60);
            assert!(allowed(&limiter, 3, later));
            assert_eq!(limiter.tracked_keys(), 1);
        }

        #[test]
        fn credential_at_10x_is_clamped_and_other_credentials_are_unaffected() {
            let limiter = KeyedRateLimiter::new(Limits::new(
                CREDENTIAL_REQUESTS_PER_SECOND,
                CREDENTIAL_BURST,
            ));
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
            let limiter = KeyedRateLimiter::with_max_tracked(Limits::new(1, 1), 2);
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
            let limiter = KeyedRateLimiter::with_max_tracked(Limits::new(100, 1), 2);
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

        fn app(requests_per_second: u32, burst: u32) -> Router {
            let state = RateLimitState::for_test(requests_per_second, burst);
            Router::new()
                .route("/test", get(|| async { "ok" }))
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

        fn bearer(token: &str) -> String {
            format!("Bearer {token}")
        }

        #[tokio::test]
        async fn credential_limit_returns_429_with_retry_after() {
            let app = app(0, 1);
            let first = app
                .clone()
                .oneshot(request(Some(&bearer("token-a"))))
                .await
                .unwrap();
            let limited = app
                .clone()
                .oneshot(request(Some(&bearer("token-a"))))
                .await
                .unwrap();

            assert_eq!(first.status(), StatusCode::OK);
            assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                limited.headers().get(http::header::RETRY_AFTER).unwrap(),
                "1"
            );
        }

        #[tokio::test]
        async fn credentials_are_limited_independently() {
            let app = app(0, 1);
            let first = app
                .clone()
                .oneshot(request(Some(&bearer("token-a"))))
                .await
                .unwrap();
            let limited = app
                .clone()
                .oneshot(request(Some(&bearer("token-a"))))
                .await
                .unwrap();
            let other = app
                .clone()
                .oneshot(request(Some(&bearer("token-b"))))
                .await
                .unwrap();

            assert_eq!(first.status(), StatusCode::OK);
            assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(other.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn bearer_scheme_is_case_insensitive_and_whitespace_tolerant() {
            let app = app(0, 1);
            let first = app
                .clone()
                .oneshot(request(Some("Bearer token-a")))
                .await
                .unwrap();
            let limited = app
                .clone()
                .oneshot(request(Some("bearer  token-a")))
                .await
                .unwrap();

            assert_eq!(first.status(), StatusCode::OK);
            assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        }

        #[tokio::test]
        async fn requests_without_bearer_credentials_pass_through() {
            let app = app(0, 1);
            for authorization in [
                None,
                Some("Basic dXNlcjpwYXNz"),
                Some("token-without-scheme"),
            ] {
                for _ in 0..3 {
                    let response = app.clone().oneshot(request(authorization)).await.unwrap();
                    assert_eq!(response.status(), StatusCode::OK, "{authorization:?}");
                }
            }
        }

        #[tokio::test]
        async fn rotating_credentials_each_get_a_fresh_bucket() {
            // Known gap: with no per-peer limit, a client that invents a new
            // token per request is never limited here. Each token still costs
            // one bounded validation, and the kid-miss JWKS guard still applies.
            let app = app(0, 1);
            for token in ["token-a", "token-b", "token-c"] {
                let response = app
                    .clone()
                    .oneshot(request(Some(&bearer(token))))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK, "{token}");
            }
        }
    }
}
