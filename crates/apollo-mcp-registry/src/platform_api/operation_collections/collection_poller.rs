use futures::Stream;
use graphql_client::GraphQLQuery;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;

use super::{error::CollectionError, event::CollectionEvent};
use crate::platform_api::PlatformApiConfig;
use operation_collection_default_polling_query::{
    OperationCollectionDefaultPollingQueryVariant as PollingDefaultGraphVariant,
    OperationCollectionDefaultPollingQueryVariantOnGraphVariantMcpDefaultCollection as PollingDefaultCollection,
};
use operation_collection_default_query::{
    OperationCollectionDefaultQueryVariant,
    OperationCollectionDefaultQueryVariantOnGraphVariantMcpDefaultCollection as DefaultCollectionResult,
    OperationCollectionDefaultQueryVariantOnGraphVariantMcpDefaultCollectionOnOperationCollectionOperations as OperationCollectionDefaultEntry,
};
use operation_collection_entries_query::OperationCollectionEntriesQueryOperationCollectionEntries;
use operation_collection_polling_query::{
    OperationCollectionPollingQueryOperationCollection as PollingOperationCollectionResult,
    OperationCollectionPollingQueryOperationCollectionOnNotFoundError as PollingNotFoundError,
    OperationCollectionPollingQueryOperationCollectionOnPermissionError as PollingPermissionError,
    OperationCollectionPollingQueryOperationCollectionOnValidationError as PollingValidationError,
};
use operation_collection_query::{
    OperationCollectionQueryOperationCollection as OperationCollectionResult,
    OperationCollectionQueryOperationCollectionOnNotFoundError as NotFoundError,
    OperationCollectionQueryOperationCollectionOnOperationCollectionOperations as OperationCollectionEntry,
    OperationCollectionQueryOperationCollectionOnPermissionError as PermissionError,
    OperationCollectionQueryOperationCollectionOnValidationError as ValidationError,
};

const MAX_COLLECTION_SIZE_FOR_POLLING: usize = 100;
const RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 10;

type Timestamp = String;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/operation_collections/operation_collections.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize, Clone"
)]
struct OperationCollectionEntriesQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/operation_collections/operation_collections.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct OperationCollectionPollingQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/operation_collections/operation_collections.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct OperationCollectionQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/operation_collections/operation_collections.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct OperationCollectionDefaultQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/operation_collections/operation_collections.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct OperationCollectionDefaultPollingQuery;

async fn handle_poll_result(
    previous_updated_at: &mut HashMap<String, OperationData>,
    poll: Vec<(String, String)>,
    platform_api_config: &PlatformApiConfig,
) -> Result<Option<Vec<OperationData>>, CollectionError> {
    let removed_ids = previous_updated_at.clone();
    let removed_ids = removed_ids
        .keys()
        .filter(|id| poll.iter().all(|(keep_id, _)| keep_id != *id))
        .collect::<Vec<_>>();

    let changed_ids: Vec<String> = poll
        .into_iter()
        .filter_map(|(id, last_updated_at)| match previous_updated_at.get(&id) {
            Some(previous_operation) if last_updated_at == previous_operation.last_updated_at => {
                None
            }
            _ => Some(id.clone()),
        })
        .collect();

    if changed_ids.is_empty() && removed_ids.is_empty() {
        tracing::debug!("no operation changed");
        return Ok(None);
    }

    if !removed_ids.is_empty() {
        tracing::info!("removed operation ids: {:?}", removed_ids);
        for id in removed_ids {
            previous_updated_at.remove(id);
        }
    }

    if !changed_ids.is_empty() {
        tracing::debug!("changed operation ids: {:?}", changed_ids);
        let full_response = graphql_request::<OperationCollectionEntriesQuery>(
            &OperationCollectionEntriesQuery::build_query(
                operation_collection_entries_query::Variables {
                    collection_entry_ids: changed_ids,
                },
            ),
            platform_api_config,
        )
        .await?;
        for operation in full_response.operation_collection_entries {
            previous_updated_at.insert(
                operation.id.clone(),
                OperationData::from(&operation).clone(),
            );
        }
    }

    Ok(Some(previous_updated_at.clone().into_values().collect()))
}

fn is_collection_error_transient(error: &CollectionError) -> bool {
    match error {
        CollectionError::Request(req_err) => {
            // Check if the underlying reqwest error is transient
            req_err.is_connect()
                || req_err.is_timeout()
                || req_err.is_request()
                || req_err.status().is_some_and(|status| {
                    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                })
        }
        _ => false,
    }
}

trait InitialCollectionFetcher: Send {
    type Operations: Send;

    fn fetch(
        &self,
        config: &PlatformApiConfig,
    ) -> impl std::future::Future<Output = Result<Self::Operations, CollectionError>> + Send;

    fn description(&self) -> &'static str;
}

struct ByIdFetcher {
    collection_id: String,
}

impl InitialCollectionFetcher for ByIdFetcher {
    type Operations = Vec<OperationCollectionEntry>;

    async fn fetch(&self, config: &PlatformApiConfig) -> Result<Self::Operations, CollectionError> {
        let response = graphql_request::<OperationCollectionQuery>(
            &OperationCollectionQuery::build_query(operation_collection_query::Variables {
                operation_collection_id: self.collection_id.clone(),
            }),
            config,
        )
        .await?;

        match response.operation_collection {
            OperationCollectionResult::NotFoundError(NotFoundError { message })
            | OperationCollectionResult::PermissionError(PermissionError { message })
            | OperationCollectionResult::ValidationError(ValidationError { message }) => {
                Err(CollectionError::Response(message))
            }
            OperationCollectionResult::OperationCollection(collection) => Ok(collection.operations),
        }
    }

    fn description(&self) -> &'static str {
        "collection"
    }
}

struct ByDefaultFetcher {
    graph_ref: String,
}

impl InitialCollectionFetcher for ByDefaultFetcher {
    type Operations = Vec<OperationCollectionDefaultEntry>;

    async fn fetch(&self, config: &PlatformApiConfig) -> Result<Self::Operations, CollectionError> {
        let response = graphql_request::<OperationCollectionDefaultQuery>(
            &OperationCollectionDefaultQuery::build_query(
                operation_collection_default_query::Variables {
                    graph_ref: self.graph_ref.clone(),
                },
            ),
            config,
        )
        .await?;

        match response.variant {
            Some(OperationCollectionDefaultQueryVariant::GraphVariant(variant)) => {
                match variant.mcp_default_collection {
                    DefaultCollectionResult::OperationCollection(collection) => {
                        Ok(collection.operations)
                    }
                    DefaultCollectionResult::PermissionError(error) => {
                        Err(CollectionError::Response(error.message))
                    }
                }
            }
            Some(OperationCollectionDefaultQueryVariant::InvalidRefFormat(err)) => {
                Err(CollectionError::Response(err.message))
            }
            None => Err(CollectionError::Response(format!(
                "{} not found",
                self.graph_ref
            ))),
        }
    }

    fn description(&self) -> &'static str {
        "default collection"
    }
}

async fn retry_initial_fetch<F: InitialCollectionFetcher>(
    fetcher: &F,
    sender: &tokio::sync::mpsc::Sender<CollectionEvent>,
    config: &PlatformApiConfig,
) -> Option<F::Operations> {
    for attempt in 1..=MAX_RETRIES {
        if sender.is_closed() {
            tracing::debug!("Sender closed during startup retry, shutting down");
            return None;
        }

        match fetcher.fetch(config).await {
            Ok(operations) => return Some(operations),
            Err(err) if is_collection_error_transient(&err) => {
                if attempt == MAX_RETRIES {
                    tracing::error!(
                        event = "collection_startup_failed",
                        error = %err,
                        attempts = MAX_RETRIES,
                        "Initial {} fetch failed after retries, server will shutdown",
                        fetcher.description()
                    );
                    sender
                        .send(CollectionEvent::CollectionError(err))
                        .await
                        .ok();
                    return None;
                }
                tracing::warn!(
                    attempt,
                    max_retries = MAX_RETRIES,
                    "Failed to fetch initial {} (transient error), retrying in {:?}: {}",
                    fetcher.description(),
                    RETRY_DELAY,
                    err
                );
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(err) => {
                tracing::error!(
                    "Failed to fetch initial {} with permanent error: {}",
                    fetcher.description(),
                    err
                );
                sender
                    .send(CollectionEvent::CollectionError(err))
                    .await
                    .ok();
                return None;
            }
        }
    }
    None
}

#[derive(Clone)]
pub struct OperationData {
    id: String,
    last_updated_at: String,
    pub source_text: String,
    pub headers: Option<Vec<(String, String)>>,
    pub variables: Option<String>,
}
impl From<&OperationCollectionEntry> for OperationData {
    fn from(operation: &OperationCollectionEntry) -> Self {
        Self {
            id: operation.id.clone(),
            last_updated_at: operation.last_updated_at.clone(),
            source_text: operation
                .operation_data
                .current_operation_revision
                .body
                .clone(),
            headers: operation
                .operation_data
                .current_operation_revision
                .headers
                .as_ref()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|h| (h.name.clone(), h.value.clone()))
                        .collect()
                }),
            variables: operation
                .operation_data
                .current_operation_revision
                .variables
                .clone(),
        }
    }
}
impl From<&OperationCollectionEntriesQueryOperationCollectionEntries> for OperationData {
    fn from(operation: &OperationCollectionEntriesQueryOperationCollectionEntries) -> Self {
        Self {
            id: operation.id.clone(),
            last_updated_at: operation.last_updated_at.clone(),
            source_text: operation
                .operation_data
                .current_operation_revision
                .body
                .clone(),
            headers: operation
                .operation_data
                .current_operation_revision
                .headers
                .as_ref()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|h| (h.name.clone(), h.value.clone()))
                        .collect()
                }),
            variables: operation
                .operation_data
                .current_operation_revision
                .variables
                .clone(),
        }
    }
}
impl From<&OperationCollectionDefaultEntry> for OperationData {
    fn from(operation: &OperationCollectionDefaultEntry) -> Self {
        Self {
            id: operation.id.clone(),
            last_updated_at: operation.last_updated_at.clone(),
            source_text: operation
                .operation_data
                .current_operation_revision
                .body
                .clone(),
            headers: operation
                .operation_data
                .current_operation_revision
                .headers
                .as_ref()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|h| (h.name.clone(), h.value.clone()))
                        .collect()
                }),
            variables: operation
                .operation_data
                .current_operation_revision
                .variables
                .clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum CollectionSource {
    Id(String, PlatformApiConfig),
    Default(String, PlatformApiConfig),
}

async fn write_init_response(
    sender: &tokio::sync::mpsc::Sender<CollectionEvent>,
    previous_updated_at: &mut HashMap<String, OperationData>,
    operations: impl Iterator<Item = OperationData>,
) -> bool {
    let operations = operations
        .inspect(|operation_data| {
            previous_updated_at.insert(operation_data.id.clone(), operation_data.clone());
        })
        .collect::<Vec<_>>();
    let operation_count = operations.len();
    if let Err(e) = sender
        .send(CollectionEvent::UpdateOperationCollection(operations))
        .await
    {
        tracing::debug!(
            "failed to push to stream. This is likely to be because the server is shutting down: {e}"
        );
        false
    } else if operation_count > MAX_COLLECTION_SIZE_FOR_POLLING {
        tracing::warn!(
            "Operation Collection polling disabled. Collection has {} operations which exceeds the maximum of {}.",
            operation_count,
            MAX_COLLECTION_SIZE_FOR_POLLING
        );
        false
    } else {
        true
    }
}
impl CollectionSource {
    pub fn into_stream(self) -> Pin<Box<dyn Stream<Item = CollectionEvent> + Send>> {
        match self {
            CollectionSource::Id(ref id, ref platform_api_config) => {
                self.collection_id_stream(id.clone(), platform_api_config.clone())
            }
            CollectionSource::Default(ref graph_ref, ref platform_api_config) => {
                self.default_collection_stream(graph_ref.clone(), platform_api_config.clone())
            }
        }
    }

    fn collection_id_stream(
        &self,
        collection_id: String,
        platform_api_config: PlatformApiConfig,
    ) -> Pin<Box<dyn Stream<Item = CollectionEvent> + Send>> {
        let (sender, receiver) = channel(2);
        tokio::task::spawn(async move {
            let mut previous_updated_at = HashMap::new();

            // Initial fetch with retry
            let fetcher = ByIdFetcher {
                collection_id: collection_id.clone(),
            };
            let Some(operations) =
                retry_initial_fetch(&fetcher, &sender, &platform_api_config).await
            else {
                return;
            };

            let should_poll = write_init_response(
                &sender,
                &mut previous_updated_at,
                operations.iter().map(OperationData::from),
            )
            .await;
            if !should_poll {
                return;
            }

            // Polling loop
            loop {
                tokio::time::sleep(platform_api_config.poll_interval).await;

                match poll_operation_collection_id(
                    collection_id.clone(),
                    &platform_api_config,
                    &mut previous_updated_at,
                )
                .await
                {
                    Ok(Some(operations)) => {
                        let operations_count = operations.len();
                        if let Err(e) = sender
                            .send(CollectionEvent::UpdateOperationCollection(operations))
                            .await
                        {
                            tracing::debug!(
                                "failed to push to stream. This is likely to be because the server is shutting down: {e}"
                            );
                            break;
                        } else if operations_count > MAX_COLLECTION_SIZE_FOR_POLLING {
                            tracing::warn!(
                                "Operation Collection polling disabled. Collection has {operations_count} operations which exceeds the maximum of {MAX_COLLECTION_SIZE_FOR_POLLING}."
                            );
                            break;
                        }
                    }
                    Ok(None) => {
                        tracing::debug!("Operation collection unchanged");
                    }
                    Err(err) => {
                        if is_collection_error_transient(&err) {
                            // Log transient errors but don't send CollectionError to prevent server restart
                            tracing::warn!(
                                "Failed to poll operation collection (transient error), will retry on next poll in {}s: {}",
                                platform_api_config.poll_interval.as_secs(),
                                err
                            );
                        } else {
                            tracing::error!(
                                "Failed to poll operation collection with permanent error: {err}"
                            );
                            if let Err(e) = sender.send(CollectionEvent::CollectionError(err)).await
                            {
                                tracing::debug!(
                                    "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                                );
                            }
                            break;
                        }
                    }
                }
            }
        });
        Box::pin(ReceiverStream::new(receiver))
    }

    pub fn default_collection_stream(
        &self,
        graph_ref: String,
        platform_api_config: PlatformApiConfig,
    ) -> Pin<Box<dyn Stream<Item = CollectionEvent> + Send>> {
        let (sender, receiver) = channel(2);
        tokio::task::spawn(async move {
            let mut previous_updated_at = HashMap::new();

            // Initial fetch with retry
            let fetcher = ByDefaultFetcher {
                graph_ref: graph_ref.clone(),
            };
            let Some(operations) =
                retry_initial_fetch(&fetcher, &sender, &platform_api_config).await
            else {
                return;
            };

            let should_poll = write_init_response(
                &sender,
                &mut previous_updated_at,
                operations.iter().map(OperationData::from),
            )
            .await;
            if !should_poll {
                return;
            }

            // Polling loop
            loop {
                tokio::time::sleep(platform_api_config.poll_interval).await;

                match poll_operation_collection_default(
                    graph_ref.clone(),
                    &platform_api_config,
                    &mut previous_updated_at,
                )
                .await
                {
                    Ok(Some(operations)) => {
                        let operations_count = operations.len();
                        if let Err(e) = sender
                            .send(CollectionEvent::UpdateOperationCollection(operations))
                            .await
                        {
                            tracing::debug!(
                                "failed to push to stream. This is likely to be because the server is shutting down: {e}"
                            );
                            break;
                        } else if operations_count > MAX_COLLECTION_SIZE_FOR_POLLING {
                            tracing::warn!(
                                "Operation Collection polling disabled. Collection has {operations_count} operations which exceeds the maximum of {MAX_COLLECTION_SIZE_FOR_POLLING}."
                            );
                            break;
                        }
                    }
                    Ok(None) => {
                        tracing::debug!("Operation collection unchanged");
                    }
                    Err(err) => {
                        if is_collection_error_transient(&err) {
                            // Log transient errors but don't send CollectionError to prevent server restart
                            tracing::warn!(
                                "Failed to poll operation collection (transient error), will retry on next poll in {}s: {}",
                                platform_api_config.poll_interval.as_secs(),
                                err
                            );
                        } else {
                            tracing::error!(
                                "Failed to poll operation collection with permanent error: {err}"
                            );
                            if let Err(e) = sender.send(CollectionEvent::CollectionError(err)).await
                            {
                                tracing::debug!(
                                    "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                                );
                            }
                            break;
                        }
                    }
                }
            }
        });
        Box::pin(ReceiverStream::new(receiver))
    }
}

async fn poll_operation_collection_id(
    collection_id: String,
    platform_api_config: &PlatformApiConfig,
    previous_updated_at: &mut HashMap<String, OperationData>,
) -> Result<Option<Vec<OperationData>>, CollectionError> {
    let response = graphql_request::<OperationCollectionPollingQuery>(
        &OperationCollectionPollingQuery::build_query(
            operation_collection_polling_query::Variables {
                operation_collection_id: collection_id.clone(),
            },
        ),
        platform_api_config,
    )
    .await?;

    match response.operation_collection {
        PollingOperationCollectionResult::OperationCollection(collection) => {
            handle_poll_result(
                previous_updated_at,
                collection
                    .operations
                    .into_iter()
                    .map(|operation| (operation.id, operation.last_updated_at))
                    .collect(),
                platform_api_config,
            )
            .await
        }
        PollingOperationCollectionResult::NotFoundError(PollingNotFoundError { message })
        | PollingOperationCollectionResult::PermissionError(PollingPermissionError { message })
        | PollingOperationCollectionResult::ValidationError(PollingValidationError { message }) => {
            Err(CollectionError::Response(message))
        }
    }
}

async fn poll_operation_collection_default(
    graph_ref: String,
    platform_api_config: &PlatformApiConfig,
    previous_updated_at: &mut HashMap<String, OperationData>,
) -> Result<Option<Vec<OperationData>>, CollectionError> {
    let response = graphql_request::<OperationCollectionDefaultPollingQuery>(
        &OperationCollectionDefaultPollingQuery::build_query(
            operation_collection_default_polling_query::Variables { graph_ref },
        ),
        platform_api_config,
    )
    .await?;

    match response.variant {
        Some(PollingDefaultGraphVariant::GraphVariant(variant)) => {
            match variant.mcp_default_collection {
                PollingDefaultCollection::OperationCollection(collection) => {
                    handle_poll_result(
                        previous_updated_at,
                        collection
                            .operations
                            .into_iter()
                            .map(|operation| (operation.id, operation.last_updated_at))
                            .collect(),
                        platform_api_config,
                    )
                    .await
                }

                PollingDefaultCollection::PermissionError(error) => {
                    Err(CollectionError::Response(error.message))
                }
            }
        }
        Some(PollingDefaultGraphVariant::InvalidRefFormat(err)) => {
            Err(CollectionError::Response(err.message))
        }
        None => Err(CollectionError::Response(
            "Default collection not found".to_string(),
        )),
    }
}

async fn graphql_request<Query>(
    request_body: &graphql_client::QueryBody<Query::Variables>,
    platform_api_config: &PlatformApiConfig,
) -> Result<Query::ResponseData, CollectionError>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: std::fmt::Debug,
{
    let res = reqwest::Client::new()
        .post(platform_api_config.registry_url.clone())
        .headers(HeaderMap::from_iter(vec![
            (
                HeaderName::from_static("apollographql-client-name"),
                HeaderValue::from_static("apollo-mcp-server"),
            ),
            (
                HeaderName::from_static("apollographql-client-version"),
                HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
            ),
            (
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(platform_api_config.apollo_key.expose_secret())
                    .map_err(CollectionError::HeaderValue)?,
            ),
        ]))
        .timeout(platform_api_config.timeout)
        .json(request_body)
        .send()
        .await
        .map_err(CollectionError::Request)?;

    let response_body: graphql_client::Response<Query::ResponseData> =
        res.json().await.map_err(CollectionError::Request)?;
    response_body
        .data
        .ok_or(CollectionError::Response("missing data".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_collection_error_transient_response_error() {
        // Response errors are NOT transient (e.g., invalid graph ref, permission denied)
        let err = CollectionError::Response("graph not found".to_string());
        assert!(!is_collection_error_transient(&err));
    }

    #[test]
    fn test_is_collection_error_transient_header_errors() {
        // Header name errors are NOT transient (configuration issues)
        let err = CollectionError::HeaderName(
            reqwest::header::HeaderName::from_bytes(b"invalid header").unwrap_err(),
        );
        assert!(!is_collection_error_transient(&err));
    }

    #[test]
    fn test_is_collection_error_transient_invalid_variables() {
        // Invalid variables are NOT transient (user error)
        let err = CollectionError::InvalidVariables("bad json".to_string());
        assert!(!is_collection_error_transient(&err));
    }

    // Mock fetcher for testing retry logic
    struct MockFetcher {
        results: std::sync::Mutex<Vec<Result<String, CollectionError>>>,
    }

    impl InitialCollectionFetcher for MockFetcher {
        type Operations = String;

        async fn fetch(
            &self,
            _config: &PlatformApiConfig,
        ) -> Result<Self::Operations, CollectionError> {
            self.results.lock().unwrap().remove(0)
        }

        fn description(&self) -> &'static str {
            "mock"
        }
    }

    fn test_config() -> PlatformApiConfig {
        PlatformApiConfig {
            registry_url: "http://localhost".parse().unwrap(),
            timeout: std::time::Duration::from_secs(5),
            poll_interval: std::time::Duration::from_secs(10),
            apollo_key: "test".to_string().into(),
        }
    }

    #[tokio::test]
    async fn test_retry_initial_fetch_success_first_try() {
        let fetcher = MockFetcher {
            results: std::sync::Mutex::new(vec![Ok("data".to_string())]),
        };
        let (sender, _receiver) = tokio::sync::mpsc::channel::<CollectionEvent>(1);
        let config = test_config();

        let result = retry_initial_fetch(&fetcher, &sender, &config).await;
        assert_eq!(result, Some("data".to_string()));
    }

    #[tokio::test]
    async fn test_retry_initial_fetch_permanent_error_no_retry() {
        let fetcher = MockFetcher {
            results: std::sync::Mutex::new(vec![Err(CollectionError::Response(
                "not found".to_string(),
            ))]),
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<CollectionEvent>(1);
        let config = test_config();

        let result = retry_initial_fetch(&fetcher, &sender, &config).await;
        assert!(result.is_none());

        // Should have sent a CollectionError
        let event = receiver.try_recv();
        assert!(matches!(event, Ok(CollectionEvent::CollectionError(_))));
    }
}
