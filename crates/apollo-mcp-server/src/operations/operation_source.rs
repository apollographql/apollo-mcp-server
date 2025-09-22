use std::{
    collections::HashMap,
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
use tracing::warn;

use crate::event::Event;

use super::RawOperation;

const OPERATION_DOCUMENT_EXTENSION: &str = "graphql";

/// The source of the operations exposed as MCP tools
#[derive(Clone, Debug)]
pub enum OperationSource {
    /// GraphQL document files
    Files(Vec<PathBuf>),

    /// Persisted Query manifest
    Manifest(ManifestSource),

    /// Operation collection
    Collection(CollectionSource),

    /// No operations provided
    None,
}

impl OperationSource {
    #[tracing::instrument(skip_all, fields(operation_source = ?self))]
    pub async fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            OperationSource::Files(paths) => Self::stream_file_changes(paths).boxed(),
            OperationSource::Manifest(manifest_source) => manifest_source
                .into_stream()
                .await
                .map(|event| {
                    let ManifestEvent::UpdateManifest(operations) = event;
                    Event::OperationsUpdated(
                        operations.into_iter().map(RawOperation::from).collect(),
                    )
                })
                .boxed(),
            OperationSource::Collection(collection_source) => collection_source
                .into_stream()
                .map(|event| match event {
                    CollectionEvent::UpdateOperationCollection(operations) => {
                        match operations
                            .iter()
                            .map(RawOperation::try_from)
                            .collect::<Result<Vec<_>, _>>()
                        {
                            Ok(operations) => Event::OperationsUpdated(operations),
                            Err(e) => Event::CollectionError(e),
                        }
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
                                    Some(Event::OperationsUpdated(
                                        state.values().flatten().cloned().collect::<Vec<_>>(),
                                    ))
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
        OperationSource::Manifest(manifest_source)
    }
}

impl From<Vec<PathBuf>> for OperationSource {
    fn from(paths: Vec<PathBuf>) -> Self {
        OperationSource::Files(paths)
    }
}
