// tonic does not derive `Eq` for the gRPC message types, which causes a warning from Clippy. The
// current suggestion is to explicitly allow the lint in the module that imports the protos.
// Read more: https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use crate::uplink::UplinkRequest;
use crate::uplink::UplinkResponse;
use crate::uplink::schema::SchemaState;
use crate::uplink::schema::schema_stream::supergraph_sdl_query::FetchErrorCode;
use crate::uplink::schema::schema_stream::supergraph_sdl_query::SupergraphSdlQueryRouterConfig;
use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/uplink/schema/schema_query.graphql",
    schema_path = "src/uplink/uplink.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize",
    deprecated = "warn"
)]
pub(crate) struct SupergraphSdlQuery;

impl From<UplinkRequest> for supergraph_sdl_query::Variables {
    fn from(req: UplinkRequest) -> Self {
        supergraph_sdl_query::Variables {
            api_key: req.api_key,
            graph_ref: req.graph_ref,
            if_after_id: req.id,
        }
    }
}

impl From<supergraph_sdl_query::ResponseData> for UplinkResponse<String> {
    fn from(response: supergraph_sdl_query::ResponseData) -> Self {
        match response.router_config {
            SupergraphSdlQueryRouterConfig::RouterConfigResult(result) => UplinkResponse::New {
                response: result.supergraph_sdl,
                id: result.id,
                // this will truncate the number of seconds to under u64::MAX, which should be
                // a large enough delay anyway
                delay: result.min_delay_seconds as u64,
            },
            SupergraphSdlQueryRouterConfig::Unchanged(response) => UplinkResponse::Unchanged {
                id: Some(response.id),
                delay: Some(response.min_delay_seconds as u64),
            },
            SupergraphSdlQueryRouterConfig::FetchError(err) => UplinkResponse::Error {
                retry_later: err.code == FetchErrorCode::RETRY_LATER,
                code: match err.code {
                    FetchErrorCode::AUTHENTICATION_FAILED => "AUTHENTICATION_FAILED".to_string(),
                    FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                    FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                    FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                    FetchErrorCode::NOT_IMPLEMENTED_ON_THIS_INSTANCE => {
                        "NOT_IMPLEMENTED_ON_THIS_INSTANCE".to_string()
                    }
                    FetchErrorCode::Other(other) => other,
                },
                message: err.message,
            },
        }
    }
}

impl From<supergraph_sdl_query::ResponseData> for UplinkResponse<SchemaState> {
    fn from(response: supergraph_sdl_query::ResponseData) -> Self {
        match response.router_config {
            SupergraphSdlQueryRouterConfig::RouterConfigResult(result) => UplinkResponse::New {
                response: SchemaState {
                    sdl: result.supergraph_sdl,
                    launch_id: Some(result.id.clone()),
                },
                id: result.id,
                // this will truncate the number of seconds to under u64::MAX, which should be
                // a large enough delay anyway
                delay: result.min_delay_seconds as u64,
            },
            SupergraphSdlQueryRouterConfig::Unchanged(response) => UplinkResponse::Unchanged {
                id: Some(response.id),
                delay: Some(response.min_delay_seconds as u64),
            },
            SupergraphSdlQueryRouterConfig::FetchError(err) => UplinkResponse::Error {
                retry_later: err.code == FetchErrorCode::RETRY_LATER,
                code: match err.code {
                    FetchErrorCode::AUTHENTICATION_FAILED => "AUTHENTICATION_FAILED".to_string(),
                    FetchErrorCode::ACCESS_DENIED => "ACCESS_DENIED".to_string(),
                    FetchErrorCode::UNKNOWN_REF => "UNKNOWN_REF".to_string(),
                    FetchErrorCode::RETRY_LATER => "RETRY_LATER".to_string(),
                    FetchErrorCode::NOT_IMPLEMENTED_ON_THIS_INSTANCE => {
                        "NOT_IMPLEMENTED_ON_THIS_INSTANCE".to_string()
                    }
                    FetchErrorCode::Other(other) => other,
                },
                message: err.message,
            },
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_uplink_request_to_graphql_variables() {
        let request = UplinkRequest {
            api_key: "test_key".to_string(),
            graph_ref: "test_ref".to_string(),
            id: Some("test_id".to_string()),
        };

        let variables: supergraph_sdl_query::Variables = request.into();

        assert_eq!(variables.api_key, "test_key");
        assert_eq!(variables.graph_ref, "test_ref");
        assert_eq!(variables.if_after_id, Some("test_id".to_string()));
    }

    #[test]
    fn test_graphql_response_to_uplink_response_new() {
        let response = supergraph_sdl_query::ResponseData {
            router_config: SupergraphSdlQueryRouterConfig::RouterConfigResult(
                supergraph_sdl_query::SupergraphSdlQueryRouterConfigOnRouterConfigResult {
                    supergraph_sdl: "test_sdl".to_string(),
                    id: "result_id".to_string(),
                    min_delay_seconds: 42.0,
                },
            ),
        };

        let uplink_response: UplinkResponse<String> = response.into();

        assert!(matches!(
            uplink_response,
            UplinkResponse::New { response, id, delay }
            if response == "test_sdl" && id == "result_id" && delay == 42
        ));
    }

    #[test]
    fn test_graphql_response_to_uplink_response_unchanged() {
        let response = supergraph_sdl_query::ResponseData {
            router_config: SupergraphSdlQueryRouterConfig::Unchanged(
                supergraph_sdl_query::SupergraphSdlQueryRouterConfigOnUnchanged {
                    id: "unchanged_id".to_string(),
                    min_delay_seconds: 30.0,
                },
            ),
        };

        let uplink_response: UplinkResponse<String> = response.into();

        assert!(matches!(
            uplink_response,
            UplinkResponse::Unchanged { id, delay }
            if id == Some("unchanged_id".to_string()) && delay == Some(30)
        ));
    }

    #[test]
    fn test_graphql_response_to_uplink_response_error() {
        let response = supergraph_sdl_query::ResponseData {
            router_config: SupergraphSdlQueryRouterConfig::FetchError(
                supergraph_sdl_query::SupergraphSdlQueryRouterConfigOnFetchError {
                    code: FetchErrorCode::RETRY_LATER,
                    message: "Try again later".to_string(),
                },
            ),
        };

        let uplink_response: UplinkResponse<String> = response.into();

        assert!(matches!(
            uplink_response,
            UplinkResponse::Error { retry_later, code, message }
            if retry_later && code == "RETRY_LATER" && message == "Try again later"
        ));
    }
}
