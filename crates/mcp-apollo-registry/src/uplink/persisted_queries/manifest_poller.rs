use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use futures::prelude::*;
use parking_lot::RwLock;
use reqwest::Client;
use tokio::fs::read_to_string;
use tokio::sync::mpsc;
use tower::BoxError;

use crate::uplink::UplinkConfig;
use crate::uplink::persisted_queries::manifest::FullPersistedQueryOperationId;
use crate::uplink::persisted_queries::manifest::PersistedQueryManifest;
use crate::uplink::persisted_queries::manifest::SignedUrlChunk;
use crate::uplink::persisted_queries::{
    MaybePersistedQueriesManifestChunks, PersistedQueriesManifestChunk,
    PersistedQueriesManifestQuery,
};
use crate::uplink::stream_from_uplink_transforming_new_response;

/// Holds the current state of persisted queries
#[derive(Debug)]
pub struct PersistedQueryManifestPollerState {
    /// The current persisted query manifest
    pub persisted_query_manifest: PersistedQueryManifest,
}

/// The result of the first time build of the persisted query manifest.
#[derive(Debug)]
pub enum ManifestPollResultOnStartup {
    LoadedOperations,
    Err(BoxError),
}

/// A notification when the persisted query manifest changes.
pub struct ManifestChanged;

#[derive(Debug)]
pub enum ManifestSource {
    LocalStatic(Vec<PathBuf>),
    LocalHotReload(Vec<PathBuf>),
    Uplink(UplinkConfig),
}

/// Manages polling uplink for persisted query chunks and unpacking those chunks into a [`PersistedQueryManifest`].
#[derive(Clone, Debug)]
pub struct PersistedQueryManifestPoller {
    pub state: Arc<RwLock<PersistedQueryManifestPollerState>>,
    _drop_signal: mpsc::Sender<()>,
}

impl PersistedQueryManifestPoller {
    /// Create a new [`PersistedQueryManifestPoller`] from an UplinkConfig.
    /// Starts polling immediately and this function only returns after all chunks have been fetched
    /// and the [`PersistedQueryManifest`] has been fully populated.
    pub async fn new(
        manifest_source: ManifestSource,
        change_sender: mpsc::Sender<ManifestChanged>,
    ) -> Result<Self, BoxError> {
        let manifest_stream = create_manifest_stream(manifest_source).await?;

        // Initialize state
        let state = Arc::new(RwLock::new(PersistedQueryManifestPollerState {
            persisted_query_manifest: PersistedQueryManifest::default(),
        }));

        // Start the background polling task
        let (_drop_signal, drop_receiver) = mpsc::channel::<()>(1);
        let (ready_sender, mut ready_receiver) = mpsc::channel::<ManifestPollResultOnStartup>(1);

        let state_clone = state.clone();

        tokio::task::spawn(async move {
            poll_manifest_stream(
                manifest_stream,
                state_clone,
                ready_sender,
                change_sender,
                drop_receiver,
            )
            .await;
        });

        match ready_receiver.recv().await {
            Some(ManifestPollResultOnStartup::LoadedOperations) => Ok(Self {
                state,
                _drop_signal,
            }),
            Some(ManifestPollResultOnStartup::Err(e)) => Err(e),
            None => Err("could not receive ready event for persisted query layer".into()),
        }
    }

    /// Get the operation body associated with the given persisted query ID and optional client name
    pub fn get_operation_body(
        &self,
        persisted_query_id: &str,
        client_name: Option<String>,
    ) -> Option<String> {
        let state = self.state.read();
        if let Some(body) = state
            .persisted_query_manifest
            .get(&FullPersistedQueryOperationId {
                operation_id: persisted_query_id.to_string(),
                client_name: client_name.clone(),
            })
            .cloned()
        {
            Some(body)
        } else if client_name.is_some() {
            state
                .persisted_query_manifest
                .get(&FullPersistedQueryOperationId {
                    operation_id: persisted_query_id.to_string(),
                    client_name: None,
                })
                .cloned()
        } else {
            None
        }
    }

    /// Get all operations in the manifest
    pub fn get_all_operations(&self) -> Vec<(String, String)> {
        let state = self.state.read();
        state
            .persisted_query_manifest
            .iter()
            .map(|(k, v)| (k.operation_id.clone(), v.clone()))
            .collect()
    }
}

async fn manifest_from_uplink_chunks(
    new_chunks: Vec<PersistedQueriesManifestChunk>,
    http_client: Client,
) -> Result<PersistedQueryManifest, BoxError> {
    let mut new_persisted_query_manifest = PersistedQueryManifest::default();
    tracing::debug!("ingesting new persisted queries: {:?}", &new_chunks);
    // TODO: consider doing these fetches in parallel
    for new_chunk in new_chunks {
        fetch_chunk_into_manifest(
            new_chunk,
            &mut new_persisted_query_manifest,
            http_client.clone(),
        )
        .await?
    }

    tracing::info!(
        "Loaded {} persisted queries.",
        new_persisted_query_manifest.len()
    );

    Ok(new_persisted_query_manifest)
}

async fn fetch_chunk_into_manifest(
    chunk: PersistedQueriesManifestChunk,
    manifest: &mut PersistedQueryManifest,
    http_client: Client,
) -> Result<(), BoxError> {
    let mut it = chunk.urls.iter().peekable();
    while let Some(chunk_url) = it.next() {
        match fetch_chunk(http_client.clone(), chunk_url).await {
            Ok(chunk) => {
                manifest.add_chunk(&chunk);
                return Ok(());
            }
            Err(e) => {
                if it.peek().is_some() {
                    // There's another URL to try, so log as debug and move on.
                    tracing::debug!(
                        "failed to fetch persisted query list chunk from {}: {}. \
                         Other endpoints will be tried",
                        chunk_url,
                        e
                    );
                    continue;
                } else {
                    // No more URLs; fail the function.
                    return Err(e);
                }
            }
        }
    }
    // The loop always returns unless there's another iteration after it, so the
    // only way we can fall off the loop is if we never entered it.
    Err("persisted query chunk did not include any URLs to fetch operations from".into())
}

async fn fetch_chunk(http_client: Client, chunk_url: &String) -> Result<SignedUrlChunk, BoxError> {
    let chunk = http_client
        .get(chunk_url.clone())
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| -> BoxError {
            format!(
                "error fetching persisted queries manifest chunk from {}: {}",
                chunk_url, e
            )
            .into()
        })?
        .json::<SignedUrlChunk>()
        .await
        .map_err(|e| -> BoxError {
            format!(
                "error reading body of persisted queries manifest chunk from {}: {}",
                chunk_url, e
            )
            .into()
        })?;

    chunk.validate()
}

/// A stream of manifest updates
type ManifestStream = dyn Stream<Item = Result<PersistedQueryManifest, BoxError>> + Send + 'static;

async fn create_manifest_stream(
    source: ManifestSource,
) -> Result<Pin<Box<ManifestStream>>, BoxError> {
    match source {
        ManifestSource::LocalStatic(paths) => Ok(stream::once(load_local_manifests(paths)).boxed()),
        ManifestSource::LocalHotReload(paths) => Ok(create_hot_reload_stream(paths).boxed()),
        ManifestSource::Uplink(uplink_config) => {
            let client = Client::builder()
                .timeout(uplink_config.timeout)
                .gzip(true)
                .build()?;
            Ok(create_uplink_stream(uplink_config, client).boxed())
        }
    }
}

async fn poll_manifest_stream(
    mut manifest_stream: Pin<Box<ManifestStream>>,
    state: Arc<RwLock<PersistedQueryManifestPollerState>>,
    ready_sender: mpsc::Sender<ManifestPollResultOnStartup>,
    change_sender: mpsc::Sender<ManifestChanged>,
    mut drop_receiver: mpsc::Receiver<()>,
) {
    let mut ready_sender = Some(ready_sender);

    loop {
        tokio::select! {
            manifest_result = manifest_stream.next() => {
                match manifest_result {
                    Some(Ok(new_manifest)) => {
                        let operation_count = new_manifest.len();
                        *state.write() = PersistedQueryManifestPollerState {
                            persisted_query_manifest: new_manifest,
                        };
                        tracing::info!("persisted query manifest successfully updated ({} operations total)", operation_count);

                        if let Some(sender) = ready_sender.take() {
                            let _ = sender.send(ManifestPollResultOnStartup::LoadedOperations).await;
                        }
                        let _ = change_sender.send(ManifestChanged).await;
                    }
                    Some(Err(e)) => {
                        tracing::error!("Error polling manifest: {}", e);
                        if let Some(sender) = ready_sender.take() {
                            let _ = sender.send(ManifestPollResultOnStartup::Err(e)).await;
                        }
                    }
                    None => break,
                }
            }
            _ = drop_receiver.recv() => break,
        }
    }
}

async fn load_local_manifests(paths: Vec<PathBuf>) -> Result<PersistedQueryManifest, BoxError> {
    let mut complete_manifest = PersistedQueryManifest::default();

    for path in paths.iter() {
        let raw_file_contents = read_to_string(path).await.map_err(|e| -> BoxError {
            format!(
                "Failed to read persisted query list file at path: {}, {}",
                path.to_string_lossy(),
                e
            )
            .into()
        })?;

        let chunk = SignedUrlChunk::parse_and_validate(&raw_file_contents)?;
        complete_manifest.add_chunk(&chunk);
    }

    tracing::info!(
        "Loaded {} persisted queries from local files.",
        complete_manifest.len()
    );

    Ok(complete_manifest)
}

fn create_uplink_stream(
    uplink_config: UplinkConfig,
    http_client: Client,
) -> impl Stream<Item = Result<PersistedQueryManifest, BoxError>> {
    stream_from_uplink_transforming_new_response::<
        PersistedQueriesManifestQuery,
        MaybePersistedQueriesManifestChunks,
        Option<PersistedQueryManifest>,
    >(uplink_config, move |response| {
        let http_client = http_client.clone();
        Box::new(Box::pin(async move {
            match response {
                Some(chunks) => manifest_from_uplink_chunks(chunks, http_client)
                    .await
                    .map(Some)
                    .map_err(|e| -> BoxError { e }),
                None => Ok(None),
            }
        }))
    })
    .filter_map(|result| async move {
        match result {
            Ok(Some(manifest)) => Some(Ok(manifest)),
            Ok(None) => None,
            Err(e) => Some(Err(e.into())),
        }
    })
}

fn create_hot_reload_stream(
    paths: Vec<PathBuf>,
) -> impl Stream<Item = Result<PersistedQueryManifest, BoxError>> {
    // Create file watchers for each path
    let file_watchers = paths.into_iter().map(|raw_path| {
        crate::files::watch(raw_path.as_ref()).then(move |_| {
            let path = raw_path.clone();
            async move {
                match read_to_string(&path).await {
                    Ok(raw_file_contents) => {
                        match SignedUrlChunk::parse_and_validate(&raw_file_contents) {
                            Ok(chunk) => Ok((path, chunk)),
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(e.into()),
                }
            }
            .boxed()
        })
    });

    // We need to keep track of the local manifest chunks so we can replace them when
    // they change.
    let mut chunks: HashMap<String, SignedUrlChunk> = HashMap::new();

    // Combine all watchers into a single stream
    stream::select_all(file_watchers).map(move |result| {
        result.map(|(path, chunk)| {
            tracing::info!(
                "hot reloading persisted query manifest file at path: {}",
                path.to_string_lossy()
            );
            chunks.insert(path.to_string_lossy().to_string(), chunk);

            let mut manifest = PersistedQueryManifest::default();
            for chunk in chunks.values() {
                manifest.add_chunk(chunk);
            }

            manifest
        })
    })
}
