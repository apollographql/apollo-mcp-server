use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use url::Url;

use super::factory::build_graph_context;
use super::manifest::types::GraphConfig;
use super::schema_oci::fetch_schema_text;
use super::staging::{GraphStagingState, StagingMap, evict_expired};
use crate::operations::MutationMode;

/// Config for a single graph, loaded from a per-graph YAML file in the watched directory.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerGraphFileConfig {
    pub name: String,
    pub endpoint: Url,
    /// OCI reference for the schema SDL in Zot, e.g.
    /// `zot.zot.svc.cluster.local:5050/schemas/graph-A:abc123`
    pub schema_ref: String,
    pub sha: String,
}

pub fn parse_per_graph_config(yaml: &str) -> Result<PerGraphFileConfig, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

/// Spawn a background task that polls `dir` every `interval`, detecting new or changed
/// per-graph YAML files. For each changed file, spawns a staging task that fetches the
/// schema from Zot, builds a GraphContext, and moves the graph to `staged` state.
/// Returns immediately; the task runs for the lifetime of the process.
pub fn watch_and_stage(
    dir: std::path::PathBuf,
    staging: StagingMap,
    interval: Duration,
    index_memory_bytes: usize,
) {
    let staging_ttl = Duration::from_secs(
        std::env::var("APOLLO_MCP_STAGING_TTL_MINUTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(15)
            * 60,
    );

    tokio::spawn(async move {
        let mut seen: std::collections::HashMap<std::path::PathBuf, String> =
            std::collections::HashMap::new();
        loop {
            // Evict expired staged entries on each poll cycle.
            evict_expired(&staging, staging_ttl).await;

            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                        continue;
                    }
                    let Ok(content) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    if seen.get(&path).map(|s| s == &content).unwrap_or(false) {
                        continue; // unchanged
                    }

                    let Ok(cfg) = parse_per_graph_config(&content) else {
                        tracing::warn!(?path, "failed to parse per-graph config, skipping");
                        continue;
                    };

                    // Check if already staged or staging at this sha.
                    {
                        let staging_map = staging.read().await;
                        match staging_map.get(&cfg.name) {
                            Some(GraphStagingState::Staged { sha, .. }) if sha == &cfg.sha => {
                                // Already staged at this sha: update seen and skip.
                                seen.insert(path.clone(), content.clone());
                                continue
                            }
                            Some(GraphStagingState::Staging) => {
                                // Currently staging: do NOT update seen (file may change again).
                                continue
                            }
                            _ => {}
                        }
                    }

                    // Update seen, mark as staging, and spawn task.
                    seen.insert(path.clone(), content.clone());

                    // Mark as staging.
                    {
                        let mut staging_map = staging.write().await;
                        staging_map.insert(cfg.name.clone(), GraphStagingState::Staging);
                    }

                    // Spawn the staging task.
                    let staging_clone = staging.clone();
                    let cfg_clone = cfg.clone();
                    tokio::spawn(async move {
                        let result = stage_graph(cfg_clone.clone(), index_memory_bytes).await;
                        let mut map = staging_clone.write().await;
                        match result {
                            Ok(context) => {
                                map.insert(
                                    cfg_clone.name.clone(),
                                    GraphStagingState::Staged {
                                        sha: cfg_clone.sha.clone(),
                                        context: Arc::new(context),
                                        staged_at: tokio::time::Instant::now(),
                                    },
                                );
                                tracing::info!(
                                    graph = %cfg_clone.name,
                                    sha = %cfg_clone.sha,
                                    "graph staged successfully"
                                );
                            }
                            Err(e) => {
                                map.insert(
                                    cfg_clone.name.clone(),
                                    GraphStagingState::Error {
                                        message: e.to_string(),
                                    },
                                );
                                tracing::error!(
                                    graph = %cfg_clone.name,
                                    error = %e,
                                    "staging failed"
                                );
                            }
                        }
                    });
                }
            }
            tokio::time::sleep(interval).await;
        }
    });
}

async fn stage_graph(
    cfg: PerGraphFileConfig,
    index_memory_bytes: usize,
) -> anyhow::Result<super::context::GraphContext> {
    tracing::info!(
        graph = %cfg.name,
        sha = %cfg.sha,
        "fetching schema from OCI for staging"
    );
    let schema_text = fetch_schema_text(&cfg.schema_ref).await?;

    // Write schema to a temp file (build_graph_context reads from PathBuf).
    let tmp = tempfile::NamedTempFile::with_suffix(".graphql")?;
    std::fs::write(tmp.path(), &schema_text)?;

    let graph_config = GraphConfig {
        name: cfg.name.clone(),
        endpoint: cfg.endpoint.clone(),
        schema: tmp.path().to_path_buf(),
        operations: vec![],
        headers: Default::default(),
        upstream_auth: None,
    };

    let ctx = build_graph_context(
        graph_config,
        index_memory_bytes,
        MutationMode::None,
        false, // disable_type_description
        false, // disable_schema_description
        false, // enable_output_schema
        &Default::default(), // annotation_overrides
        &Default::default(), // description_overrides
        None,                // custom_scalar_map
        &Default::default(), // base_headers
        &Default::default(), // base_forward_headers
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Keep tmp alive until build_graph_context has read the file.
    drop(tmp);
    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_a_valid_per_graph_config() {
        let yaml = r#"
            name: graph-A
            endpoint: http://graph-A.graph-A.svc.cluster.local:4000/graphql
            schema_ref: zot.zot.svc.cluster.local:5050/schemas/graph-A:abc123
            sha: abc123
        "#;
        let cfg = parse_per_graph_config(yaml).unwrap();
        assert_eq!(cfg.name, "graph-A");
        assert_eq!(cfg.sha, "abc123");
        assert_eq!(cfg.schema_ref, "zot.zot.svc.cluster.local:5050/schemas/graph-A:abc123");
    }

    #[test]
    fn it_rejects_unknown_fields() {
        let yaml = r#"
            name: graph-A
            endpoint: http://localhost:4000/graphql
            schema_ref: zot:5050/schemas/graph-A:abc
            sha: abc
            unknown_field: oops
        "#;
        assert!(parse_per_graph_config(yaml).is_err());
    }

    #[test]
    fn it_rejects_missing_sha() {
        let yaml = r#"
            name: graph-A
            endpoint: http://localhost:4000/graphql
            schema_ref: zot:5050/schemas/graph-A:abc
        "#;
        assert!(parse_per_graph_config(yaml).is_err());
    }
}
