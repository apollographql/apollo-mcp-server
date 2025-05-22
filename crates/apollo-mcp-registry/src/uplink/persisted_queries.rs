use graphql_client::GraphQLQuery;

pub mod event;
mod manifest;
mod manifest_poller;

pub use manifest::FullPersistedQueryOperationId;
pub use manifest::ManifestOperation;
pub use manifest::PersistedQueryManifest;
pub use manifest::SignedUrlChunk;
pub use manifest_poller::ManifestSource;
pub use manifest_poller::PersistedQueryManifestPollerState;

use crate::uplink::UplinkRequest;
use crate::uplink::UplinkResponse;

/// Persisted query manifest query definition
#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/uplink/persisted_queries/persisted_queries_manifest_query.graphql",
    schema_path = "src/uplink/uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]
pub struct PersistedQueriesManifestQuery;

impl From<UplinkRequest> for persisted_queries_manifest_query::Variables {
    fn from(req: UplinkRequest) -> Self {
        persisted_queries_manifest_query::Variables {
            api_key: req.api_key,
            graph_ref: req.graph_ref,
            if_after_id: req.id,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PersistedQueriesManifestChunk {
    pub id: String,
    pub urls: Vec<String>,
}

impl PersistedQueriesManifestChunk {
    fn from_query_chunks(
        query_chunks: &persisted_queries_manifest_query::PersistedQueriesManifestQueryPersistedQueriesOnPersistedQueriesResultChunks,
    ) -> Self {
        Self {
            id: query_chunks.id.clone(),
            urls: query_chunks.urls.clone(),
        }
    }
}

pub type PersistedQueriesManifestChunks = Vec<PersistedQueriesManifestChunk>;
pub type MaybePersistedQueriesManifestChunks = Option<PersistedQueriesManifestChunks>;

impl From<persisted_queries_manifest_query::ResponseData>
    for UplinkResponse<MaybePersistedQueriesManifestChunks>
{
    fn from(response: persisted_queries_manifest_query::ResponseData) -> Self {
        use persisted_queries_manifest_query::FetchErrorCode;
        use persisted_queries_manifest_query::PersistedQueriesManifestQueryPersistedQueries;

        match response.persisted_queries {
            PersistedQueriesManifestQueryPersistedQueries::PersistedQueriesResult(response) => {
                if let Some(chunks) = response.chunks {
                    let chunks = chunks
                        .iter()
                        .map(PersistedQueriesManifestChunk::from_query_chunks)
                        .collect();
                    UplinkResponse::New {
                        response: Some(chunks),
                        id: response.id,
                        // this will truncate the number of seconds to under u64::MAX, which should be
                        // a large enough delay anyway
                        delay: response.min_delay_seconds as u64,
                    }
                } else {
                    UplinkResponse::New {
                        // no persisted query list is associated with this variant
                        response: None,
                        id: response.id,
                        delay: response.min_delay_seconds as u64,
                    }
                }
            }
            PersistedQueriesManifestQueryPersistedQueries::Unchanged(response) => {
                UplinkResponse::Unchanged {
                    id: Some(response.id),
                    delay: Some(response.min_delay_seconds as u64),
                }
            }
            PersistedQueriesManifestQueryPersistedQueries::FetchError(err) => {
                UplinkResponse::Error {
                    retry_later: err.code == FetchErrorCode::RETRY_LATER,
                    code: match err.code {
                        FetchErrorCode::AUTHENTICATION_FAILED => {
                            "AUTHENTICATION_FAILED".to_string()
                        }
                        FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                        FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                        FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                        FetchErrorCode::NOT_IMPLEMENTED_ON_THIS_INSTANCE => {
                            "NOT_IMPLEMENTED_ON_THIS_INSTANCE".to_string()
                        }
                        FetchErrorCode::Other(other) => other,
                    },
                    message: err.message,
                }
            }
        }
    }
}
