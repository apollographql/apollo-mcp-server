//! Health Check module for Apollo MCP Server
//!
//! Provides liveness and readiness checks for the MCP server, inspired by Apollo Router's health check implementation.
//!
//! The health check is exposed via HTTP endpoints and can be used by load balancers, container orchestrators, and monitoring systems to determine server health.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::Instant;
use tracing::{debug, trace};

/// Health status enumeration
#[derive(Debug, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HealthStatus {
    Up,
    Down,
}

/// Health response structure
#[derive(Debug, Serialize)]
pub struct Health {
    status: HealthStatus,
}

/// Configuration options for the readiness health interval sub-component.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct ReadinessIntervalConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[serde(serialize_with = "humantime_serde::serialize")]
    #[schemars(with = "Option<String>", default)]
    /// The sampling interval (default: 5s)
    pub sampling: Duration,

    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[serde(serialize_with = "humantime_serde::serialize")]
    #[schemars(with = "Option<String>")]
    /// The unready interval (default: 2 * sampling interval)
    pub unready: Option<Duration>,
}

impl Default for ReadinessIntervalConfig {
    fn default() -> Self {
        Self {
            sampling: Duration::from_secs(5),
            unready: None,
        }
    }
}

/// Configuration options for the readiness health sub-component.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct ReadinessConfig {
    /// The readiness interval configuration
    pub interval: ReadinessIntervalConfig,

    /// How many rejections are allowed in an interval (default: 100)
    /// If this number is exceeded, the server will start to report unready.
    pub allowed: usize,
}

impl Default for ReadinessConfig {
    fn default() -> Self {
        Self {
            interval: Default::default(),
            allowed: 100,
        }
    }
}

/// Configuration options for the health check component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct HealthCheckConfig {
    /// Set to false to disable the health check
    pub enabled: bool,

    /// Optionally set a custom healthcheck path
    /// Defaults to /health
    pub path: String,

    /// Optionally specify readiness configuration
    pub readiness: ReadinessConfig,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: "/health".to_string(),
            readiness: Default::default(),
        }
    }
}

#[derive(Clone)]
pub struct HealthCheck {
    config: HealthCheckConfig,
    live: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    rejected: Arc<AtomicUsize>,
    ticker: Arc<tokio::task::JoinHandle<()>>,
}

impl HealthCheck {
    pub fn new(config: HealthCheckConfig) -> Self {
        let live = Arc::new(AtomicBool::new(true)); // Start as live
        let ready = Arc::new(AtomicBool::new(true)); // Start as ready
        let rejected = Arc::new(AtomicUsize::new(0));

        let allowed = config.readiness.allowed;
        let sampling_interval = config.readiness.interval.sampling;
        let recovery_interval = config
            .readiness
            .interval
            .unready
            .unwrap_or(2 * sampling_interval);

        let my_rejected = rejected.clone();
        let my_ready = ready.clone();

        let ticker = tokio::spawn(async move {
            loop {
                let start = Instant::now() + sampling_interval;
                let mut interval = tokio::time::interval_at(start, sampling_interval);
                loop {
                    interval.tick().await;
                    if my_rejected.load(Ordering::Relaxed) > allowed {
                        debug!("Health check readiness threshold exceeded, marking as unready");
                        my_ready.store(false, Ordering::SeqCst);
                        tokio::time::sleep(recovery_interval).await;
                        my_rejected.store(0, Ordering::Relaxed);
                        my_ready.store(true, Ordering::SeqCst);
                        debug!("Health check readiness restored");
                        break;
                    }
                }
            }
        });

        Self {
            config,
            live,
            ready,
            rejected,
            ticker: Arc::new(ticker),
        }
    }

    pub fn record_rejection(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn config(&self) -> &HealthCheckConfig {
        &self.config
    }

    pub fn get_health_state(&self, query: Option<&str>) -> (Health, StatusCode) {
        let mut status_code = StatusCode::OK;

        let health = if let Some(query) = query {
            let query_upper = query.to_ascii_uppercase();

            if query_upper.starts_with("READY") {
                let status = if self.ready.load(Ordering::SeqCst) {
                    HealthStatus::Up
                } else {
                    status_code = StatusCode::SERVICE_UNAVAILABLE;
                    HealthStatus::Down
                };
                Health { status }
            } else if query_upper.starts_with("LIVE") {
                let status = if self.live.load(Ordering::SeqCst) {
                    HealthStatus::Up
                } else {
                    status_code = StatusCode::SERVICE_UNAVAILABLE;
                    HealthStatus::Down
                };
                Health { status }
            } else {
                Health {
                    status: HealthStatus::Up,
                }
            }
        } else {
            Health {
                status: HealthStatus::Up,
            }
        };

        (health, status_code)
    }

    /// Enable health check router.
    ///
    /// Creates a router with the health check endpoint and merges it with the provided router.
    pub fn enable_router(&self, router: Router) -> Router {
        /// Health check endpoint handler
        async fn health_endpoint(
            State(health_check): State<HealthCheck>,
            Query(params): Query<HashMap<String, String>>,
        ) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
            let query = params.keys().next().map(|k| k.as_str());
            let (health, status_code) = health_check.get_health_state(query);

            trace!(?health, query = ?query, "health check");

            Ok((status_code, Json(json!(health))))
        }

        let health_router = Router::new()
            .route(&self.config.path, get(health_endpoint))
            .with_state(self.clone());

        router.merge(health_router)
    }
}

impl Drop for HealthCheck {
    fn drop(&mut self) {
        self.ticker.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, sleep};

    #[test]
    fn test_health_check_default_config() {
        let config = HealthCheckConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.path, "/health");
        assert_eq!(config.readiness.allowed, 100);
        assert_eq!(config.readiness.interval.sampling, Duration::from_secs(5));
        assert!(config.readiness.interval.unready.is_none());
    }

    #[tokio::test]
    async fn test_health_check_rejection_tracking() {
        let mut config = HealthCheckConfig::default();
        config.readiness.allowed = 2;
        config.readiness.interval.sampling = Duration::from_millis(50);
        config.readiness.interval.unready = Some(Duration::from_millis(100));

        let health_check = HealthCheck::new(config);

        // Should be live and ready initially
        assert!(health_check.live.load(Ordering::SeqCst));
        assert!(health_check.ready.load(Ordering::SeqCst));

        // Record rejections beyond threshold
        for _ in 0..5 {
            health_check.record_rejection();
        }

        // Wait for the ticker to process
        sleep(Duration::from_millis(100)).await;

        // Should be still live but unready now
        assert!(health_check.live.load(Ordering::SeqCst));
        assert!(!health_check.ready.load(Ordering::SeqCst));
    }
}
