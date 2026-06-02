use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

use super::context::GraphContext;

#[derive(Debug, Clone)]
pub enum GraphStagingState {
    Absent,
    Staging,
    Staged {
        sha: String,
        context: Arc<GraphContext>,
        staged_at: Instant,
    },
    Error {
        message: String,
    },
}

impl GraphStagingState {
    pub fn state_name(&self) -> &'static str {
        match self {
            GraphStagingState::Absent => "absent",
            GraphStagingState::Staging => "staging",
            GraphStagingState::Staged { .. } => "staged",
            GraphStagingState::Error { .. } => "error",
        }
    }

    pub fn sha(&self) -> Option<&str> {
        match self {
            GraphStagingState::Staged { sha, .. } => Some(sha),
            _ => None,
        }
    }
}

pub type StagingMap = Arc<RwLock<HashMap<String, GraphStagingState>>>;

pub fn new_staging_map() -> StagingMap {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Check if a staged entry is not yet expired based on its staged_at time and TTL.
fn is_not_expired(staged_at: Instant, now: Instant, ttl: Duration) -> bool {
    now.duration_since(staged_at) < ttl
}

/// Evict staged-but-not-active entries older than `ttl` from the staging map.
pub async fn evict_expired(staging: &StagingMap, ttl: Duration) {
    let mut map = staging.write().await;
    let now = Instant::now();
    map.retain(|_, state| match state {
        GraphStagingState::Staged { staged_at, .. } => {
            is_not_expired(*staged_at, now, ttl)
        }
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_names_are_correct() {
        assert_eq!(GraphStagingState::Absent.state_name(), "absent");
        assert_eq!(GraphStagingState::Staging.state_name(), "staging");
        assert_eq!(
            GraphStagingState::Error { message: "oops".into() }.state_name(),
            "error"
        );
    }

    #[test]
    fn expiry_predicate_is_correct() {
        let start = Instant::now();

        // An entry within TTL should not be expired
        assert!(is_not_expired(start, start, Duration::from_secs(1)));

        // An entry with TTL=0 is expired immediately (duration_since is >= 0)
        assert!(!is_not_expired(start, start, Duration::from_secs(0)));

        // Simulate checking the same time after advancing (still within 1s)
        // We use a safe margin by checking that a very small duration is not expired
        let almost_now = start + Duration::from_millis(500);
        assert!(is_not_expired(start, almost_now, Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn evict_expired_preserves_non_staged_entries() {
        let staging = new_staging_map();
        {
            let mut map = staging.write().await;
            map.insert(
                "graph-A".to_string(),
                GraphStagingState::Absent,
            );
            map.insert(
                "graph-B".to_string(),
                GraphStagingState::Staging,
            );
        }
        evict_expired(&staging, Duration::from_secs(1)).await;
        let map = staging.read().await;
        // Absent and Staging entries should not be evicted (only Staged entries with expired TTL).
        assert!(map.contains_key("graph-A"));
        assert!(map.contains_key("graph-B"));
    }
}
