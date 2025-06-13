use futures::{Stream, StreamExt};
use graphql_client::QueryBody;
use secrecy::ExposeSecret;
pub use secrecy::SecretString;
use std::error::Error as _;
use std::fmt::Debug;
use std::time::Duration;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;
use tower::BoxError;
use url::Url;

pub mod persisted_queries;
pub mod schema;

const GCP_URL: &str = "https://uplink.api.apollographql.com";
const AWS_URL: &str = "https://aws.uplink.api.apollographql.com";

/// Errors returned by the uplink module
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http error")]
    Http(#[from] reqwest::Error),

    #[error("fetch failed from uplink endpoint, and there are no fallback endpoints configured")]
    FetchFailedSingle,

    #[error("fetch failed from all {url_count} uplink endpoints")]
    FetchFailedMultiple { url_count: usize },

    #[allow(clippy::enum_variant_names)]
    #[error("uplink error: code={code} message={message}")]
    UplinkError { code: String, message: String },

    #[error("uplink error, the request will not be retried: code={code} message={message}")]
    UplinkErrorNoRetry { code: String, message: String },
}

/// Represents a request to Apollo Uplink
#[derive(Debug)]
pub struct UplinkRequest {
    pub api_key: String,
    pub graph_ref: String,
    pub id: Option<String>,
}

/// The response from an Apollo Uplink request
#[derive(Debug)]
pub enum UplinkResponse<Response>
where
    Response: Send + Debug + 'static,
{
    New {
        response: Response,
        id: String,
        delay: u64,
    },
    Unchanged {
        id: Option<String>,
        delay: Option<u64>,
    },
    Error {
        retry_later: bool,
        code: String,
        message: String,
    },
}

/// Endpoint configuration strategies
#[derive(Debug, Clone)]
pub enum Endpoints {
    Fallback {
        urls: Vec<Url>,
    },
    #[allow(dead_code)]
    RoundRobin {
        urls: Vec<Url>,
        current: usize,
    },
}

impl Default for Endpoints {
    #[allow(clippy::expect_used)] // Default URLs are fixed at compile-time
    fn default() -> Self {
        Self::fallback(
            [GCP_URL, AWS_URL]
                .iter()
                .map(|url| Url::parse(url).expect("default urls must be valid"))
                .collect(),
        )
    }
}

impl Endpoints {
    pub fn fallback(urls: Vec<Url>) -> Self {
        Endpoints::Fallback { urls }
    }

    pub fn round_robin(urls: Vec<Url>) -> Self {
        Endpoints::RoundRobin { urls, current: 0 }
    }

    /// Return an iterator of endpoints to check on a poll of uplink.
    /// Fallback will always return URLs in the same order.
    /// Round-robin will return an iterator that cycles over the URLS starting at the next URL
    fn iter<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a Url> + Send + 'a> {
        match self {
            Endpoints::Fallback { urls } => Box::new(urls.iter()),
            Endpoints::RoundRobin { urls, current } => {
                // Prevent current from getting large.
                *current %= urls.len();

                // The iterator cycles, but will skip to the next untried URL and is finally limited by the number of URLs.
                // This gives us a sliding window of URLs to try on each poll to uplink.
                // The returned iterator will increment current each time it is called.
                Box::new(
                    urls.iter()
                        .cycle()
                        .skip(*current)
                        .inspect(|_| {
                            *current += 1;
                        })
                        .take(urls.len()),
                )
            }
        }
    }

    pub fn url_count(&self) -> usize {
        match self {
            Endpoints::Fallback { urls } => urls.len(),
            Endpoints::RoundRobin { urls, current: _ } => urls.len(),
        }
    }
}

/// Configuration for polling Apollo Uplink.
#[derive(Clone, Debug, Default)]
pub struct UplinkConfig {
    /// The Apollo key: `<YOUR_GRAPH_API_KEY>`
    pub apollo_key: SecretString,

    /// The apollo graph reference: `<YOUR_GRAPH_ID>@<VARIANT>`
    pub apollo_graph_ref: String,

    /// The endpoints polled.
    pub endpoints: Option<Endpoints>,

    /// The duration between polling
    pub poll_interval: Duration,

    /// The HTTP client timeout for each poll
    pub timeout: Duration,
}

impl UplinkConfig {
    /// Mock uplink configuration options for use in tests
    /// A nice pattern is to use wiremock to start an uplink mocker and pass the URL here.
    pub fn for_tests(uplink_endpoints: Endpoints) -> Self {
        Self {
            apollo_key: SecretString::from("key"),
            apollo_graph_ref: "graph".to_string(),
            endpoints: Some(uplink_endpoints),
            poll_interval: Duration::from_secs(2),
            timeout: Duration::from_secs(5),
        }
    }
}

/// Regularly fetch from Uplink
/// If urls are supplied then they will be called round-robin
pub fn stream_from_uplink<Query, Response>(
    uplink_config: UplinkConfig,
) -> impl Stream<Item = Result<Response, Error>>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + 'static + Debug,
{
    stream_from_uplink_transforming_new_response::<Query, Response, Response>(
        uplink_config,
        |response| Box::new(Box::pin(async { Ok(response) })),
    )
}

/// Like stream_from_uplink, but applies an async transformation function to the
/// result of the HTTP fetch if the response is an UplinkResponse::New. If this
/// function returns Err, we fail over to the next Uplink endpoint, just like if
/// the HTTP fetch itself failed. This serves the use case where an Uplink
/// endpoint's response includes another URL located close to the Uplink
/// endpoint; if that second URL is down, we want to try the next Uplink
/// endpoint rather than fully giving up.
pub fn stream_from_uplink_transforming_new_response<Query, Response, TransformedResponse>(
    mut uplink_config: UplinkConfig,
    transform_new_response: impl Fn(
        Response,
    ) -> Box<
        dyn Future<Output = Result<TransformedResponse, BoxError>> + Send + Unpin,
    > + Send
    + Sync
    + 'static,
) -> impl Stream<Item = Result<TransformedResponse, Error>>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + 'static + Debug,
    TransformedResponse: Send + 'static + Debug,
{
    let (sender, receiver) = channel(2);
    let client = match reqwest::Client::builder()
        .no_gzip()
        .timeout(uplink_config.timeout)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!("unable to create client to query uplink: {err}", err = err);
            return futures::stream::empty().boxed();
        }
    };

    let task = async move {
        let mut last_id = None;
        let mut endpoints = uplink_config.endpoints.unwrap_or_default();
        loop {
            let variables = UplinkRequest {
                graph_ref: uplink_config.apollo_graph_ref.to_string(),
                api_key: uplink_config.apollo_key.expose_secret().to_string(),
                id: last_id.clone(),
            };

            let query_body = Query::build_query(variables.into());

            match fetch::<Query, Response, TransformedResponse>(
                &client,
                &query_body,
                &mut endpoints,
                &transform_new_response,
            )
            .await
            {
                Ok(response) => {
                    match response {
                        UplinkResponse::New {
                            id,
                            response,
                            delay,
                        } => {
                            last_id = Some(id);
                            uplink_config.poll_interval = Duration::from_secs(delay);

                            if let Err(e) = sender.send(Ok(response)).await {
                                tracing::debug!(
                                    "failed to push to stream. This is likely to be because the server is shutting down: {e}"
                                );
                                break;
                            }
                        }
                        UplinkResponse::Unchanged { id, delay } => {
                            // Preserve behavior for schema uplink errors where id and delay are not reset if they are not provided on error.
                            if let Some(id) = id {
                                last_id = Some(id);
                            }
                            if let Some(delay) = delay {
                                uplink_config.poll_interval = Duration::from_secs(delay);
                            }
                        }
                        UplinkResponse::Error {
                            retry_later,
                            message,
                            code,
                        } => {
                            let err = if retry_later {
                                Err(Error::UplinkError { code, message })
                            } else {
                                Err(Error::UplinkErrorNoRetry { code, message })
                            };
                            if let Err(e) = sender.send(err).await {
                                tracing::debug!(
                                    "failed to send error to uplink stream. This is likely to be because the server is shutting down: {e}"
                                );
                                break;
                            }
                            if !retry_later {
                                break;
                            }
                        }
                    }
                }
                Err(err) => {
                    if let Err(e) = sender.send(Err(err)).await {
                        tracing::debug!(
                            "failed to send error to uplink stream. This is likely to be because the server is shutting down: {e}"
                        );
                        break;
                    }
                }
            }

            tokio::time::sleep(uplink_config.poll_interval).await;
        }
    };

    // Using tokio::spawn instead of with_current_subscriber to simplify
    tokio::task::spawn(task);

    ReceiverStream::new(receiver).boxed()
}

async fn fetch<Query, Response, TransformedResponse>(
    client: &reqwest::Client,
    request_body: &QueryBody<Query::Variables>,
    endpoints: &mut Endpoints,
    // See stream_from_uplink_transforming_new_response for an explanation of
    // this argument.
    transform_new_response: &(
         impl Fn(
        Response,
    ) -> Box<dyn Future<Output = Result<TransformedResponse, BoxError>> + Send + Unpin>
         + Send
         + Sync
         + 'static
     ),
) -> Result<UplinkResponse<TransformedResponse>, Error>
where
    Query: graphql_client::GraphQLQuery,
    <Query as graphql_client::GraphQLQuery>::ResponseData: Into<UplinkResponse<Response>> + Send,
    <Query as graphql_client::GraphQLQuery>::Variables: From<UplinkRequest> + Send + Sync,
    Response: Send + Debug + 'static,
    TransformedResponse: Send + Debug + 'static,
{
    for url in endpoints.iter() {
        match http_request::<Query>(client, url.as_str(), request_body).await {
            Ok(response) => match response.data.map(Into::into) {
                None => {}
                Some(UplinkResponse::New {
                    response,
                    id,
                    delay,
                }) => match transform_new_response(response).await {
                    Ok(res) => {
                        return Ok(UplinkResponse::New {
                            response: res,
                            id,
                            delay,
                        });
                    }
                    Err(err) => {
                        tracing::debug!(
                            "failed to process results of Uplink response from {url}: {err}. Other endpoints will be tried"
                        );
                        continue;
                    }
                },
                Some(UplinkResponse::Unchanged { id, delay }) => {
                    return Ok(UplinkResponse::Unchanged { id, delay });
                }
                Some(UplinkResponse::Error {
                    message,
                    code,
                    retry_later,
                }) => {
                    return Ok(UplinkResponse::Error {
                        message,
                        code,
                        retry_later,
                    });
                }
            },
            Err(err) => {
                tracing::debug!(
                    "failed to fetch from Uplink endpoint {url}: {err}. Other endpoints will be tried"
                );
            }
        };
    }

    let url_count = endpoints.url_count();
    if url_count == 1 {
        Err(Error::FetchFailedSingle)
    } else {
        Err(Error::FetchFailedMultiple { url_count })
    }
}

async fn http_request<Query>(
    client: &reqwest::Client,
    url: &str,
    request_body: &QueryBody<Query::Variables>,
) -> Result<graphql_client::Response<Query::ResponseData>, reqwest::Error>
where
    Query: graphql_client::GraphQLQuery,
{
    let res = client
        .post(url)
        .header("x-apollo-mcp-server-version", env!("CARGO_PKG_VERSION"))
        .json(request_body)
        .send()
        .await
        .inspect_err(|e| {
            if let Some(hyper_err) = e.source() {
                if let Some(os_err) = hyper_err.source() {
                    if os_err.to_string().contains("tcp connect error: Cannot assign requested address (os error 99)") {
                        tracing::warn!("If your MCP server is executing within a kubernetes pod, this failure may be caused by istio-proxy injection. See https://github.com/apollographql/router/issues/3533 for more details about how to solve this");
                    }
                }
            }
        })?;
    tracing::debug!("uplink response {:?}", res);
    let response_body: graphql_client::Response<Query::ResponseData> = res.json().await?;
    Ok(response_body)
}
