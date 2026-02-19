use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use apollo_mcp_registry::{
    files,
    platform_api::operation_collections::{
        collection_poller::CollectionSource, event::CollectionEvent,
    },
    uplink::persisted_queries::{ManifestSource, event::Event as ManifestEvent},
};
use futures::{Stream, StreamExt as _};
use regex::Regex;
use tracing::warn;

use crate::event::Event;

use super::RawOperation;

const OPERATION_DOCUMENT_EXTENSION: &str = "graphql";

/// The source of the operations exposed as MCP tools
#[derive(Clone, Debug)]
pub enum OperationSource {
    /// GraphQL document files
    Files(Vec<PathBuf>),

    /// Persisted Query manifest with optional per-operation description overrides
    Manifest {
        source: ManifestSource,
        descriptions: HashMap<String, String>,
    },

    /// Operation collection
    Collection(CollectionSource),

    /// No operations provided
    None,
}

impl OperationSource {
    pub fn manifest(source: ManifestSource, descriptions: HashMap<String, String>) -> Self {
        OperationSource::Manifest {
            source,
            descriptions,
        }
    }

    #[tracing::instrument(skip_all, fields(operation_source = ?self))]
    pub async fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            OperationSource::Files(paths) => Self::stream_file_changes(paths).boxed(),
            OperationSource::Manifest {
                source,
                descriptions,
            } => source
                .into_stream()
                .await
                .map(move |event| {
                    let ManifestEvent::UpdateManifest(operations) = event;
                    Event::OperationsUpdated(
                        operations
                            .into_iter()
                            .map(|tuple| {
                                let mut raw = RawOperation::from(tuple);
                                if let Some(desc) = extract_operation_name(&raw.source_text)
                                    .and_then(|name| descriptions.get(name))
                                {
                                    raw.description = Some(desc.clone());
                                }
                                raw
                            })
                            .collect(),
                    )
                })
                .boxed(),
            OperationSource::Collection(collection_source) => collection_source
                .into_stream()
                .map(|event| match event {
                    CollectionEvent::UpdateOperationCollection(operations) => {
                        let raw_operations = operations
                            .iter()
                            .filter_map(|op| {
                                RawOperation::try_from(op)
                                    .inspect_err(|e| {
                                        warn!("Skipping invalid operation in collection: {e}");
                                    })
                                    .ok()
                            })
                            .collect();
                        Event::OperationsUpdated(raw_operations)
                    }
                    CollectionEvent::CollectionError(error) => Event::CollectionError(error),
                })
                .boxed(),
            OperationSource::None => {
                futures::stream::once(async { Event::OperationsUpdated(vec![]) }).boxed()
            }
        }
    }

    #[tracing::instrument]
    fn stream_file_changes(paths: Vec<PathBuf>) -> impl Stream<Item = Event> {
        let path_count = paths.len();
        let state = Arc::new(Mutex::new(HashMap::<PathBuf, Vec<RawOperation>>::new()));
        futures::stream::select_all(paths.into_iter().map(|path| {
            let state = Arc::clone(&state);
            files::watch(path.as_ref())
                .filter_map(move |_| {
                    let path = path.clone();
                    let state = Arc::clone(&state);
                    async move {
                        let mut operations = Vec::new();
                        if path.is_dir() {
                            // Handle a directory
                            if let Ok(entries) = fs::read_dir(&path) {
                                for entry in entries.flatten() {
                                    let entry_path = entry.path();
                                    if entry_path.extension().and_then(|e| e.to_str())
                                        == Some(OPERATION_DOCUMENT_EXTENSION)
                                    {
                                        match fs::read_to_string(&entry_path) {
                                            Ok(content) => {
                                                // Be forgiving of empty files in the directory case.
                                                // It likely means a new file was created in an editor,
                                                // but the operation hasn't been written yet.
                                                if !content.trim().is_empty() {
                                                    operations.push(RawOperation::from((
                                                        content,
                                                        entry_path.to_str().map(|s| s.to_string()),
                                                    )));
                                                }
                                            }
                                            Err(e) => {
                                                return Some(Event::OperationError(
                                                    e,
                                                    path.to_str().map(|s| s.to_string()),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            // Handle a single file
                            match fs::read_to_string(&path) {
                                Ok(content) => {
                                    if !content.trim().is_empty() {
                                        operations.push(RawOperation::from((
                                            content,
                                            path.to_str().map(|s| s.to_string()),
                                        )));
                                    } else {
                                        warn!(?path, "Empty operation file");
                                    }
                                }
                                Err(e) => {
                                    return Some(Event::OperationError(
                                        e,
                                        path.to_str().map(|s| s.to_string()),
                                    ));
                                }
                            }
                        }
                        match state.lock() {
                            Ok(mut state) => {
                                state.insert(path.clone(), operations);
                                // All paths send an initial event on startup. To avoid repeated
                                // operation events on startup, wait until all paths have been
                                // loaded, then send a single event with the operations for all
                                // paths.
                                if state.len() == path_count {
                                    // Deduplicate operations by their canonical source path
                                    let mut seen_paths = HashSet::new();
                                    let deduplicated_operations: Vec<RawOperation> = state
                                        .values()
                                        .flatten()
                                        .filter(|op| {
                                            if let Some(source_path) = &op.source_path {
                                                // Try to canonicalize the path, fall back to the original if it fails
                                                let canonical_path = PathBuf::from(source_path)
                                                    .canonicalize()
                                                    .unwrap_or_else(|_| PathBuf::from(source_path));
                                                let is_new =
                                                    seen_paths.insert(canonical_path.clone());
                                                if !is_new {
                                                    tracing::debug!(
                                                        ?canonical_path,
                                                        "Filtered duplicate operation"
                                                    );
                                                }
                                                is_new
                                            } else {
                                                // If there's no source path, include the operation
                                                true
                                            }
                                        })
                                        .cloned()
                                        .collect();
                                    Some(Event::OperationsUpdated(deduplicated_operations))
                                } else {
                                    None
                                }
                            }
                            Err(_) => Some(Event::OperationError(
                                std::io::Error::other("State mutex poisoned"),
                                path.to_str().map(|s| s.to_string()),
                            )),
                        }
                    }
                })
                .boxed()
        }))
        .boxed()
    }
}

impl From<ManifestSource> for OperationSource {
    fn from(manifest_source: ManifestSource) -> Self {
        OperationSource::Manifest {
            source: manifest_source,
            descriptions: HashMap::new(),
        }
    }
}

#[allow(clippy::expect_used)]
static OP_NAME_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?:query|mutation|subscription)\s+([A-Za-z_]\w*)")
        .expect("OP_NAME_RE is a valid regex")
});

/// Extract the operation name from a GraphQL operation body using a lightweight
/// regex, avoiding a full parse. Returns `None` for anonymous operations.
fn extract_operation_name(source_text: &str) -> Option<&str> {
    OP_NAME_RE
        .captures(source_text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
}

impl From<Vec<PathBuf>> for OperationSource {
    fn from(paths: Vec<PathBuf>) -> Self {
        OperationSource::Files(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::env::temp_dir;
    use std::fs;
    use std::io::Write;

    #[tokio::test]
    async fn test_deduplication_of_overlapping_paths() {
        let temp_base = temp_dir();
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tools_dir = temp_base.join(format!("test_dedup_{}", test_id));
        fs::create_dir(&tools_dir).unwrap();

        let operation_file = tools_dir.join("TestOperation.graphql");
        let mut file = fs::File::create(&operation_file).unwrap();
        writeln!(file, "query TestOperation {{ __typename }}").unwrap();
        drop(file);

        let paths = vec![tools_dir.clone(), operation_file.clone()];

        let operation_source = OperationSource::Files(paths);
        let mut stream = operation_source.into_stream().await;

        if let Some(Event::OperationsUpdated(operations)) = stream.next().await {
            assert_eq!(
                operations.len(),
                1,
                "Expected 1 operation after deduplication, but got {}",
                operations.len()
            );

            assert!(operations[0].source_path.is_some());
            let source_path = operations[0].source_path.as_ref().unwrap();
            assert!(
                source_path.ends_with("TestOperation.graphql"),
                "Expected source path to end with TestOperation.graphql, got: {}",
                source_path
            );
        } else {
            panic!("Expected OperationsUpdated event");
        }

        let _ = fs::remove_dir_all(&tools_dir);
    }

    #[test]
    fn extract_name_from_query() {
        assert_eq!(
            extract_operation_name("query GetAlerts($state: String!) { alerts { severity } }"),
            Some("GetAlerts")
        );
    }

    #[test]
    fn extract_name_from_mutation() {
        assert_eq!(
            extract_operation_name(
                "mutation CreateUser($name: String!) { createUser(name: $name) { id } }"
            ),
            Some("CreateUser")
        );
    }

    #[test]
    fn extract_name_from_subscription() {
        assert_eq!(
            extract_operation_name("subscription OnMessage { messages { text } }"),
            Some("OnMessage")
        );
    }

    #[test]
    fn extract_name_with_leading_comment() {
        assert_eq!(
            extract_operation_name("# Get alerts\nquery GetAlerts { alerts { severity } }"),
            Some("GetAlerts")
        );
    }

    #[test]
    fn extract_name_returns_none_for_anonymous() {
        assert_eq!(extract_operation_name("{ alerts { severity } }"), None);
    }

    #[test]
    fn extract_name_returns_none_for_anonymous_query() {
        assert_eq!(
            extract_operation_name("query { alerts { severity } }"),
            None
        );
    }

    #[tokio::test]
    async fn manifest_descriptions_applied_to_raw_operations() {
        let manifest_json = r#"{
            "format": "apollo-persisted-query-manifest",
            "version": 1,
            "operations": [
                {
                    "id": "abc123",
                    "body": "query GetAlerts($state: String!) { alerts(state: $state) { severity } }"
                },
                {
                    "id": "def456",
                    "body": "query GetForecast($coord: String!) { forecast(coord: $coord) { temp } }"
                }
            ]
        }"#;

        let temp_dir = temp_dir();
        let test_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let manifest_path = temp_dir.join(format!("test_manifest_{}.json", test_id));
        let mut file = fs::File::create(&manifest_path).unwrap();
        file.write_all(manifest_json.as_bytes()).unwrap();
        drop(file);

        let mut descriptions = HashMap::new();
        descriptions.insert(
            "GetAlerts".to_string(),
            "Get weather alerts for a US state".to_string(),
        );

        let source = OperationSource::manifest(
            ManifestSource::LocalStatic(vec![manifest_path.clone()]),
            descriptions,
        );
        let mut stream = source.into_stream().await;

        if let Some(Event::OperationsUpdated(operations)) = stream.next().await {
            assert_eq!(operations.len(), 2);

            let alerts_op = operations
                .iter()
                .find(|op| op.source_text.contains("GetAlerts"));
            let forecast_op = operations
                .iter()
                .find(|op| op.source_text.contains("GetForecast"));

            assert_eq!(
                alerts_op.unwrap().description.as_deref(),
                Some("Get weather alerts for a US state"),
                "GetAlerts should have the config-provided description"
            );
            assert!(
                forecast_op.unwrap().description.is_none(),
                "GetForecast should have no description (not in config)"
            );
        } else {
            panic!("Expected OperationsUpdated event");
        }

        let _ = fs::remove_file(&manifest_path);
    }
}
