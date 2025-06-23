use futures::Stream;
use graphql_client::GraphQLQuery;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::pin::Pin;
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

#[derive(Clone)]
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
            match graphql_request::<OperationCollectionQuery>(
                &OperationCollectionQuery::build_query(operation_collection_query::Variables {
                    operation_collection_id: collection_id.clone(),
                }),
                &platform_api_config,
            )
            .await
            {
                Ok(response) => match response.operation_collection {
                    OperationCollectionResult::NotFoundError(NotFoundError { message })
                    | OperationCollectionResult::PermissionError(PermissionError { message })
                    | OperationCollectionResult::ValidationError(ValidationError { message }) => {
                        if let Err(e) = sender
                            .send(CollectionEvent::CollectionError(CollectionError::Response(
                                message,
                            )))
                            .await
                        {
                            tracing::debug!(
                                "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                            );
                            return;
                        }
                    }
                    OperationCollectionResult::OperationCollection(collection) => {
                        let should_poll = write_init_response(
                            &sender,
                            &mut previous_updated_at,
                            collection.operations.iter().map(OperationData::from),
                        )
                        .await;
                        if !should_poll {
                            return;
                        }
                    }
                },
                Err(err) => {
                    if let Err(e) = sender.send(CollectionEvent::CollectionError(err)).await {
                        tracing::debug!(
                            "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                        );
                    }
                    return;
                }
            };

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
                        if let Err(e) = sender.send(CollectionEvent::CollectionError(err)).await {
                            tracing::debug!(
                                "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                            );
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
            match graphql_request::<OperationCollectionDefaultQuery>(
                &OperationCollectionDefaultQuery::build_query(
                    operation_collection_default_query::Variables {
                        graph_ref: graph_ref.clone(),
                    },
                ),
                &platform_api_config,
            )
            .await
            {
                Ok(response) => match response.variant {
                    Some(OperationCollectionDefaultQueryVariant::GraphVariant(variant)) => {
                        match variant.mcp_default_collection {
                            DefaultCollectionResult::OperationCollection(collection) => {
                                let should_poll = write_init_response(
                                    &sender,
                                    &mut previous_updated_at,
                                    collection.operations.iter().map(OperationData::from),
                                )
                                .await;
                                if !should_poll {
                                    return;
                                }
                            }
                            DefaultCollectionResult::PermissionError(error) => {
                                if let Err(e) = sender
                                    .send(CollectionEvent::CollectionError(
                                        CollectionError::Response(error.message),
                                    ))
                                    .await
                                {
                                    tracing::debug!(
                                        "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                                    );
                                    return;
                                }
                            }
                        }
                    }
                    Some(OperationCollectionDefaultQueryVariant::InvalidRefFormat(err)) => {
                        if let Err(e) = sender
                            .send(CollectionEvent::CollectionError(CollectionError::Response(
                                err.message,
                            )))
                            .await
                        {
                            tracing::debug!(
                                "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                            );
                            return;
                        }
                    }
                    None => {
                        if let Err(e) = sender
                            .send(CollectionEvent::CollectionError(CollectionError::Response(
                                format!("{graph_ref} not found"),
                            )))
                            .await
                        {
                            tracing::debug!(
                                "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                            );
                        }
                        return;
                    }
                },
                Err(err) => {
                    if let Err(e) = sender.send(CollectionEvent::CollectionError(err)).await {
                        tracing::debug!(
                            "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                        );
                    }
                    return;
                }
            };

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
                        if let Err(e) = sender.send(CollectionEvent::CollectionError(err)).await {
                            tracing::debug!(
                                "failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                            );
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
