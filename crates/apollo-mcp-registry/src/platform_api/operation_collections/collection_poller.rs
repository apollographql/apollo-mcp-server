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
use operation_collection_entries_query::OperationCollectionEntriesQueryOperationCollectionEntries;
use operation_collection_polling_query::{
    OperationCollectionPollingQueryOperationCollection,
    OperationCollectionPollingQueryOperationCollectionOnOperationCollection,
};

const PLATFORM_API: &str = "https://graphql.api.apollographql.com/api/graphql";
const MAX_COLLECTION_SIZE_FOR_POLLING: usize = 100;

type Timestamp = String;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/operation_collections/operation_collection_entries_query.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize, Clone"
)]
struct OperationCollectionEntriesQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/operation_collections/operation_collection_polling_query.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct OperationCollectionPollingQuery;

fn changed_ids(
    previous_updated_at: &mut HashMap<String, CollectionCache>,
    poll: OperationCollectionPollingQueryOperationCollectionOnOperationCollection,
) -> Vec<String> {
    poll.operations
        .iter()
        .filter_map(|operation| {
            let updated_at = operation.last_updated_at.clone();
            if let Some(previous_operation) = previous_updated_at.get(&operation.id) {
                if updated_at == *previous_operation.last_updated_at {
                    None
                } else {
                    previous_updated_at.insert(
                        operation.id.clone(),
                        CollectionCache {
                            last_updated_at: updated_at,
                            operation_data: previous_operation.operation_data.clone(),
                        },
                    );
                    Some(operation.id.clone())
                }
            } else {
                previous_updated_at.insert(
                    operation.id.clone(),
                    CollectionCache {
                        last_updated_at: updated_at,
                        operation_data: None,
                    },
                );
                Some(operation.id.clone())
            }
        })
        .collect()
}

#[derive(Clone)]
pub struct OperationData {
    pub source_text: String,
    pub headers: Option<Vec<(String, String)>>,
    pub variables: Option<String>,
}

#[derive(Clone)]
pub struct CollectionCache {
    last_updated_at: String,
    operation_data: Option<OperationData>,
}

impl From<&OperationCollectionEntriesQueryOperationCollectionEntries> for OperationData {
    fn from(operation: &OperationCollectionEntriesQueryOperationCollectionEntries) -> Self {
        Self {
            source_text: operation.current_operation_revision.body.clone(),
            headers: operation
                .current_operation_revision
                .headers
                .as_ref()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|h| (h.name.clone(), h.value.clone()))
                        .collect()
                }),
            variables: operation.current_operation_revision.variables.clone(),
        }
    }
}

#[derive(Clone)]
pub struct CollectionSource {
    pub collection_id: String,
    pub platform_api_config: PlatformApiConfig,
}

impl CollectionSource {
    pub fn into_stream(self) -> Pin<Box<dyn Stream<Item = CollectionEvent> + Send>> {
        let (sender, receiver) = channel(2);
        let collection_id = self.collection_id;
        let platform_api_config = self.platform_api_config;
        let task = async move {
            let mut previous_updated_at = HashMap::new();
            loop {
                match poll_operation_collection(
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
                        tracing::info!("Operation collection unchanged");
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

                tokio::time::sleep(platform_api_config.poll_interval).await;
            }
        };

        tokio::task::spawn(task);

        Box::pin(ReceiverStream::new(receiver))
    }
}

async fn graphql_request<Query>(
    request_body: &graphql_client::QueryBody<Query::Variables>,
    platform_api_config: &PlatformApiConfig,
) -> Result<Query::ResponseData, CollectionError>
where
    Query: graphql_client::GraphQLQuery,
{
    let res = reqwest::Client::new()
        .post(PLATFORM_API)
        .headers(HeaderMap::from_iter(vec![
            (
                HeaderName::from_static("apollographql-client-name"),
                HeaderValue::from_static("apollo-mcp-server"),
            ),
            // TODO: add apollographql-client-version header
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

async fn poll_operation_collection(
    collection_id: String,
    platform_api_config: &PlatformApiConfig,
    previous_updated_at: &mut HashMap<String, CollectionCache>,
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
        OperationCollectionPollingQueryOperationCollection::OperationCollection(collection) => {
            let changed_ids = changed_ids(previous_updated_at, collection);

            if changed_ids.is_empty() {
                tracing::debug!("no operation changed");
                Ok(None)
            } else {
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

                let mut updated_operations = HashMap::new();
                for (id, collection_data) in previous_updated_at.clone() {
                    if let Some(operation_data) = collection_data.operation_data.as_ref() {
                        updated_operations.insert(id, operation_data.clone());
                    }
                }

                for operation in full_response.operation_collection_entries {
                    let operation_id = operation.id.clone();
                    let operation_data = OperationData::from(&operation);
                    previous_updated_at.insert(
                        operation_id.clone(),
                        CollectionCache {
                            last_updated_at: operation.last_updated_at,
                            operation_data: Some(operation_data.clone()),
                        },
                    );
                    updated_operations.insert(operation_id.clone(), operation_data.clone());
                }

                Ok(Some(updated_operations.into_values().collect()))
            }
        }
        OperationCollectionPollingQueryOperationCollection::NotFoundError(error) => {
            Err(CollectionError::Response(error.message))
        }
        OperationCollectionPollingQueryOperationCollection::PermissionError(error) => {
            Err(CollectionError::Response(error.message))
        }
        OperationCollectionPollingQueryOperationCollection::ValidationError(error) => {
            Err(CollectionError::Response(error.message))
        }
    }
}
