use crate::errors::CollectionError;
use crate::operations::RawOperation;
use futures::Stream;
use graphql_client::{GraphQLQuery, Response};
use operation_collection_polling_query::{
    OperationCollectionPollingQueryOperationCollection as OPPollingQueryOP, // this is to long and break formatting.
    OperationCollectionPollingQueryOperationCollectionOnOperationCollection,
};
use operation_collection_query::OperationCollectionQueryOperationCollectionEntries;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::serde_json::{self, Value};
use std::collections::HashMap;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;

const STUDIO_API: &str = "https://graphql.api.apollographql.com/api/graphql";

/// Configuration for polling Apollo Platform API.
#[derive(Clone, Debug, Default)]
pub struct PlatformApiConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: String,

    /// The duration between polling
    pub poll_interval: Duration,
}

#[derive(Clone)]
pub struct CollectionSource {
    pub collection_id: String,
    pub platform_api_config: PlatformApiConfig,
}

impl CollectionSource {
    pub fn into_stream(self) -> Pin<Box<dyn Stream<Item = CollectionEvent> + Send>> {
        let (sender, receiver) = channel(2);
        let collection_id = self.collection_id.clone();
        let platform_api_config = self.platform_api_config.clone();
        let task = async move {
            let mut previous_updated_at = HashMap::new();
            loop {
                match poll_operation_collection(
                    collection_id.clone(),
                    &platform_api_config.apollo_key,
                    &mut previous_updated_at,
                )
                .await
                {
                    Ok(Some(operations)) => {
                        if let Err(e) = sender
                            .send(CollectionEvent::OperationCollectionUpdate(operations))
                            .await
                        {
                            tracing::debug!(
                                "failed to push to stream. This is likely to be because the server is shutting down: {e}"
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

                tokio::time::sleep(platform_api_config.poll_interval).await;
            }
        };

        tokio::task::spawn(task);

        Box::pin(ReceiverStream::new(receiver))
    }
}

pub enum CollectionEvent {
    OperationCollectionUpdate(Vec<RawOperation>),
    CollectionError(CollectionError),
}

type Timestamp = String;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/operation_collection_query.graphql",
    schema_path = "src/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize, Clone"
)]
struct OperationCollectionQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/operation_collection_polling_query.graphql",
    schema_path = "src/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct OperationCollectionPollingQuery;

fn changed_ids(
    previous_updated_at: &mut HashMap<String, CollectionData>,
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
                        CollectionData {
                            last_updated_at: updated_at,
                            raw_operation: previous_operation.raw_operation.clone(),
                        },
                    );
                    Some(operation.id.clone())
                }
            } else {
                previous_updated_at.insert(
                    operation.id.clone(),
                    CollectionData {
                        last_updated_at: updated_at,
                        raw_operation: None,
                    },
                );
                Some(operation.id.clone())
            }
        })
        .collect()
}

#[derive(Clone)]
pub struct CollectionData {
    last_updated_at: String,
    raw_operation: Option<RawOperation>,
}
async fn poll_operation_collection(
    collection_id: String,
    apollo_key: &str,
    previous_updated_at: &mut HashMap<String, CollectionData>,
) -> Result<Option<Vec<RawOperation>>, CollectionError> {
    let key_header_value =
        HeaderValue::from_str(apollo_key).map_err(CollectionError::HeaderValue)?;

    let response = reqwest::Client::new()
        .post(STUDIO_API)
        .headers(HeaderMap::from_iter(vec![
            (
                HeaderName::from_static("apollographql-client-name"),
                HeaderValue::from_static("apollo-mcp-server"),
            ),
            // TODO: add apollographql-client-version header
            (HeaderName::from_static("x-api-key"), key_header_value),
        ]))
        .json(&OperationCollectionPollingQuery::build_query(
            operation_collection_polling_query::Variables {
                operation_collection_id: collection_id.clone(),
            },
        ))
        .send()
        .await
        .map_err(CollectionError::Request)?
        .json::<Response<operation_collection_polling_query::ResponseData>>()
        .await
        .map_err(CollectionError::Request)?
        .data
        .ok_or(CollectionError::Response("missing data".to_string()))?;

    match response.operation_collection {
        OPPollingQueryOP::OperationCollection(collection) => {
            let changed_ids = changed_ids(previous_updated_at, collection);

            if changed_ids.is_empty() {
                tracing::info!("no operation changed");
                Ok(None)
            } else {
                tracing::info!("changed operation ids: {:?}", changed_ids);
                let full_response = fetch_operation_collection(changed_ids, apollo_key)
                    .await?
                    .data
                    .ok_or(CollectionError::Response("missing data".to_string()))?;

                let mut updated_operations = HashMap::new();
                for (id, collection_data) in previous_updated_at.clone() {
                    if let Some(raw_operation) = collection_data.raw_operation.as_ref() {
                        updated_operations.insert(id, raw_operation.clone());
                    }
                }

                for operation in full_response.operation_collection_entries {
                    let operation_id = operation.id.clone();
                    let raw_operation = RawOperation::try_from(&operation)?;
                    previous_updated_at.insert(
                        operation_id.clone(),
                        CollectionData {
                            last_updated_at: operation.last_updated_at,
                            raw_operation: Some(raw_operation.clone()),
                        },
                    );
                    updated_operations.insert(operation_id.clone(), raw_operation.clone());
                }

                Ok(Some(updated_operations.into_values().collect()))
            }
        }
        OPPollingQueryOP::NotFoundError(error) => Err(CollectionError::Response(error.message)),
        OPPollingQueryOP::PermissionError(error) => Err(CollectionError::Response(error.message)),
        OPPollingQueryOP::ValidationError(error) => Err(CollectionError::Response(error.message)),
    }
}

pub async fn fetch_operation_collection(
    collection_entry_ids: Vec<String>,
    apollo_key: &str,
) -> Result<Response<operation_collection_query::ResponseData>, CollectionError> {
    let key_header_value =
        HeaderValue::from_str(apollo_key).map_err(CollectionError::HeaderValue)?;

    reqwest::Client::new()
        .post(STUDIO_API)
        .headers(HeaderMap::from_iter(vec![
            (
                HeaderName::from_static("apollographql-client-name"),
                HeaderValue::from_static("apollo-mcp-server"),
            ),
            // TODO: add apollographql-client-version header
            (HeaderName::from_static("x-api-key"), key_header_value),
        ]))
        .json(&OperationCollectionQuery::build_query(
            operation_collection_query::Variables {
                collection_entry_ids,
            },
        ))
        .send()
        .await
        .map_err(CollectionError::Request)?
        .json::<Response<operation_collection_query::ResponseData>>()
        .await
        .map_err(CollectionError::Request)
}

impl TryFrom<&OperationCollectionQueryOperationCollectionEntries> for RawOperation {
    type Error = CollectionError;

    fn try_from(
        operation: &OperationCollectionQueryOperationCollectionEntries,
    ) -> Result<Self, Self::Error> {
        let variables =
            if let Some(variables) = operation.current_operation_revision.variables.as_ref() {
                if variables.trim().is_empty() {
                    Some(HashMap::new())
                } else {
                    Some(
                        serde_json::from_str::<HashMap<String, Value>>(variables)
                            .map_err(|_| CollectionError::InvalidVariables(variables.clone()))?,
                    )
                }
            } else {
                None
            };

        let headers = if let Some(headers) = operation.current_operation_revision.headers.as_ref() {
            let mut header_map = HeaderMap::new();
            for header in headers {
                header_map.insert(
                    HeaderName::from_str(&header.name).map_err(CollectionError::HeaderName)?,
                    HeaderValue::from_str(&header.value).map_err(CollectionError::HeaderValue)?,
                );
            }
            Some(header_map)
        } else {
            None
        };

        Ok(Self {
            source_text: operation.current_operation_revision.body.clone(),
            persisted_query_id: None,
            headers,
            variables,
        })
    }
}
