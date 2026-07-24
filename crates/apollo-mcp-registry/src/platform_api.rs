use reqwest::header::{HeaderMap, HeaderName, HeaderValue, InvalidHeaderValue};
use secrecy::{ExposeSecret, SecretString};
use std::fmt::Debug;
use std::sync::OnceLock;
use std::time::Duration;
use url::Url;

pub mod operation_collections;
pub mod schema;

const DEFAULT_PLATFORM_API: &str = "https://graphql.api.apollographql.com/api/graphql";

/// Errors returned by Platform API requests
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error(transparent)]
    HeaderValue(InvalidHeaderValue),

    #[error(transparent)]
    Request(reqwest::Error),

    #[error("Error in response: {0}")]
    Response(String),
}

impl RequestError {
    /// Returns `true` if the error is transient according to the Platform API fetch policy.
    pub fn is_transient(&self) -> bool {
        matches!(self, RequestError::Request(req_err) if
            req_err.is_connect()
            || req_err.is_timeout()
            || req_err.is_request()
            || req_err.status().is_some_and(|status| {
                status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            })
        )
    }
}

/// Shared HTTP client so polling requests reuse connections.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Send a GraphQL request to the Platform API and deserialize the response data.
pub(crate) async fn graphql_request<Query>(
    request_body: &graphql_client::QueryBody<Query::Variables>,
    platform_api_config: &PlatformApiConfig,
) -> Result<Query::ResponseData, RequestError>
where
    Query: graphql_client::GraphQLQuery,
{
    let res = http_client()
        .post(platform_api_config.registry_url.clone())
        .headers(HeaderMap::from_iter([
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
                    .map_err(RequestError::HeaderValue)?,
            ),
        ]))
        .timeout(platform_api_config.timeout)
        .json(request_body)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(RequestError::Request)?;

    let response_body: graphql_client::Response<Query::ResponseData> =
        res.json().await.map_err(RequestError::Request)?;
    match response_body.data {
        Some(data) => Ok(data),
        None => Err(RequestError::Response(graphql_errors_message(
            response_body.errors,
        ))),
    }
}

/// Summarize the `errors` of a GraphQL response with no `data`. GraphOS reports
/// failures such as invalid keys as HTTP 200 with only `errors`, so these
/// messages are often the only diagnostic available.
fn graphql_errors_message(errors: Option<Vec<graphql_client::Error>>) -> String {
    let messages = errors
        .unwrap_or_default()
        .into_iter()
        .map(|error| error.message)
        .collect::<Vec<_>>();
    if messages.is_empty() {
        "missing data".to_string()
    } else {
        messages.join(", ")
    }
}

/// Configuration for polling Apollo Uplink.
#[derive(Clone, Debug)]
pub struct PlatformApiConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: SecretString,

    /// The duration between polling
    pub poll_interval: Duration,

    /// The HTTP client timeout for each poll
    pub timeout: Duration,

    /// The URL of the Apollo registry
    pub registry_url: Url,
}

impl PlatformApiConfig {
    /// Creates a new `PlatformApiConfig` with the given Apollo key and default values for other fields.
    pub fn new(
        apollo_key: SecretString,
        poll_interval: Duration,
        timeout: Duration,
        registry_url: Option<Url>,
    ) -> Self {
        Self {
            apollo_key,
            poll_interval,
            timeout,
            #[allow(clippy::expect_used)]
            registry_url: registry_url
                .unwrap_or(Url::parse(DEFAULT_PLATFORM_API).expect("default URL should be valid")),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use secrecy::{ExposeSecret, SecretString};
    use std::time::Duration;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn response_error_is_not_transient() {
        let error = RequestError::Response("permission denied".to_string());
        assert!(!error.is_transient());
    }

    #[test]
    fn header_value_error_is_not_transient() {
        let invalid_value = reqwest::header::HeaderValue::from_bytes(b"\0invalid").unwrap_err();
        let error = RequestError::HeaderValue(invalid_value);
        assert!(!error.is_transient());
    }

    #[tokio::test]
    async fn client_error_404_is_not_transient() {
        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let result = reqwest::get(mock_server.uri()).await.unwrap();
        let reqwest_error = result.error_for_status().unwrap_err();

        let error = RequestError::Request(reqwest_error);
        assert!(!error.is_transient());
    }

    #[tokio::test]
    async fn connection_error_is_transient() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(1))
            .build()
            .unwrap();

        let result = client.get("http://192.0.2.1:1").send().await;
        let reqwest_error = result.unwrap_err();

        let error = RequestError::Request(reqwest_error);
        assert!(error.is_transient());
    }

    #[tokio::test]
    async fn timeout_error_is_transient() {
        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(10)))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(1))
            .build()
            .unwrap();

        let result = client.get(mock_server.uri()).send().await;
        let reqwest_error = result.unwrap_err();
        assert!(reqwest_error.is_timeout());

        let error = RequestError::Request(reqwest_error);
        assert!(error.is_transient());
    }

    #[tokio::test]
    async fn server_error_is_transient() {
        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let result = reqwest::get(mock_server.uri()).await.unwrap();
        let reqwest_error = result.error_for_status().unwrap_err();

        let error = RequestError::Request(reqwest_error);
        assert!(error.is_transient());
    }

    #[tokio::test]
    async fn rate_limit_429_is_transient() {
        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let result = reqwest::get(mock_server.uri()).await.unwrap();
        let reqwest_error = result.error_for_status().unwrap_err();

        let error = RequestError::Request(reqwest_error);
        assert!(error.is_transient());
    }

    #[test]
    fn graphql_errors_message_joins_error_messages() {
        let errors = vec![
            graphql_client::Error {
                message: "Unauthorized".to_string(),
                locations: None,
                path: None,
                extensions: None,
            },
            graphql_client::Error {
                message: "Invalid key".to_string(),
                locations: None,
                path: None,
                extensions: None,
            },
        ];
        assert_eq!(
            graphql_errors_message(Some(errors)),
            "Unauthorized, Invalid key"
        );
    }

    #[test]
    fn graphql_errors_message_falls_back_when_no_errors() {
        assert_eq!(graphql_errors_message(None), "missing data");
    }

    #[test]
    fn platform_api_config_with_none_endpoints() {
        let config = PlatformApiConfig::new(
            SecretString::from("test_apollo_key"),
            Duration::from_secs(10),
            Duration::from_secs(5),
            None,
        );
        assert_eq!(config.apollo_key.expose_secret(), "test_apollo_key");
        assert_eq!(config.poll_interval, Duration::from_secs(10));
        assert_eq!(config.timeout, Duration::from_secs(5));
        assert_eq!(config.registry_url.to_string(), DEFAULT_PLATFORM_API);
    }
}
