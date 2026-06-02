use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::dispatch::Graphs;
use super::staging::{GraphStagingState, StagingMap};

#[derive(Serialize)]
pub struct StatusResponse {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Deserialize)]
pub struct ActivateRequest {
    pub sha: String,
}

/// Shared state for admin routes.
#[derive(Clone)]
pub struct AdminState {
    pub staging: StagingMap,
    pub active: Graphs,
    /// Tracks the active sha per graph (since GraphContext has no sha field).
    pub active_shas: Arc<RwLock<HashMap<String, String>>>,
}

impl AdminState {
    pub fn new(staging: StagingMap, active: Graphs) -> Self {
        Self {
            staging,
            active,
            active_shas: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/graphs/{graph_id}/status", get(get_status))
        .route("/graphs/{graph_id}/activate", post(activate))
        .with_state(state)
}

async fn get_status(
    Path(graph_id): Path<String>,
    State(state): State<AdminState>,
) -> Json<StatusResponse> {
    // Check if active first.
    let active_sha = {
        let shas = state.active_shas.read().await;
        shas.get(&graph_id).cloned()
    };
    if let Some(sha) = active_sha {
        return Json(StatusResponse {
            state: "active".to_string(),
            sha: Some(sha),
            message: None,
        });
    }

    // Check staging map.
    let staging = state.staging.read().await;
    match staging.get(&graph_id) {
        None | Some(GraphStagingState::Absent) => Json(StatusResponse {
            state: "absent".to_string(),
            sha: None,
            message: None,
        }),
        Some(GraphStagingState::Staging) => Json(StatusResponse {
            state: "staging".to_string(),
            sha: None,
            message: None,
        }),
        Some(GraphStagingState::Staged { sha, .. }) => Json(StatusResponse {
            state: "staged".to_string(),
            sha: Some(sha.clone()),
            message: None,
        }),
        Some(GraphStagingState::Error { message }) => Json(StatusResponse {
            state: "error".to_string(),
            sha: None,
            message: Some(message.clone()),
        }),
    }
}

async fn activate(
    Path(graph_id): Path<String>,
    State(state): State<AdminState>,
    Json(body): Json<ActivateRequest>,
) -> StatusCode {
    let mut staging = state.staging.write().await;

    // Verify the requested sha is staged.
    let is_staged = matches!(
        staging.get(&graph_id),
        Some(GraphStagingState::Staged { sha, .. }) if sha == &body.sha
    );
    if !is_staged {
        return StatusCode::CONFLICT;
    }

    // Remove from staging and unwrap Arc<GraphContext>.
    let staged = staging.remove(&graph_id).unwrap();
    let (sha, arc_ctx) = match staged {
        GraphStagingState::Staged { sha, context, .. } => (sha, context),
        _ => unreachable!(),
    };

    let ctx = match Arc::try_unwrap(arc_ctx) {
        Ok(ctx) => ctx,
        Err(_) => {
            // Unexpected extra reference — put back and fail.
            staging.insert(graph_id, GraphStagingState::Staging);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // Insert into active map and record sha.
    {
        let mut active = state.active.write().await;
        active.insert(graph_id.clone(), ctx);
    }
    {
        let mut shas = state.active_shas.write().await;
        shas.insert(graph_id, sha);
    }

    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn make_admin_state() -> AdminState {
        AdminState::new(
            super::super::staging::new_staging_map(),
            Arc::new(RwLock::new(HashMap::new())),
        )
    }

    #[tokio::test]
    async fn get_status_returns_absent_for_unknown_graph() {
        let app = admin_router(make_admin_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/graphs/graph-A/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["state"], "absent");
        assert!(json.get("sha").is_none() || json["sha"].is_null());
    }

    #[tokio::test]
    async fn activate_returns_conflict_when_sha_not_staged() {
        let app = admin_router(make_admin_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/graphs/graph-A/activate")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sha":"abc123"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn get_status_returns_staging_state() {
        let state = make_admin_state();
        {
            let mut map = state.staging.write().await;
            map.insert("graph-A".to_string(), GraphStagingState::Staging);
        }
        let app = admin_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/graphs/graph-A/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["state"], "staging");
    }
}
