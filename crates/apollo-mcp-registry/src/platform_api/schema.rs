//! Schema source backed by the GraphOS Platform API.
//!
//! Polls the latest schema publication for a graph variant. Unlike Uplink,
//! which only distributes composed supergraphs, this works for non-federated
//! (monograph) graphs, whose published schema never reaches Uplink.

use std::pin::Pin;

use futures::Stream;
use graphql_client::GraphQLQuery;
use tokio::sync::mpsc::channel;
use tokio_stream::wrappers::ReceiverStream;

use crate::platform_api::{PlatformApiConfig, RequestError, graphql_request};
use crate::uplink::schema::SchemaState;
use crate::uplink::schema::event::Event;

type GraphQLDocument = String;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/schema/schema.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct PublishedSchemaQuery;

#[derive(GraphQLQuery)]
#[graphql(
    query_path = "src/platform_api/schema/schema.graphql",
    schema_path = "src/platform_api/platform-api.graphql",
    request_derives = "Debug",
    response_derives = "PartialEq, Debug, Deserialize"
)]
struct PublishedSchemaHashQuery;

/// The latest schema publication of a graph variant
struct PublishedSchema {
    hash: String,
    document: String,
}

/// Stream schema updates for a graph variant from the GraphOS Platform API.
///
/// The latest publication is fetched once at startup, then the schema hash is
/// polled at the configured interval and the document is re-fetched only when
/// the hash changes. Transient errors are retried; a permanent error before
/// the first successful fetch ends the stream.
pub fn stream_published_schema(
    graph_ref: String,
    platform_api_config: PlatformApiConfig,
) -> Pin<Box<dyn Stream<Item = Event> + Send>> {
    let (sender, receiver) = channel(2);
    tokio::task::spawn(async move {
        let mut current_hash = loop {
            match fetch_published_schema(&graph_ref, &platform_api_config).await {
                Ok(published) => {
                    if !send_update(&sender, published.document).await {
                        return;
                    }
                    break published.hash;
                }
                Err(err) if err.is_transient() => {
                    tracing::warn!(
                        "Failed to fetch published schema with transient error, will retry: {err}"
                    );
                    tokio::time::sleep(platform_api_config.poll_interval).await;
                }
                Err(err) => {
                    tracing::error!("Failed to fetch published schema with permanent error: {err}");
                    return;
                }
            }
        };

        loop {
            tokio::time::sleep(platform_api_config.poll_interval).await;
            match fetch_published_schema_hash(&graph_ref, &platform_api_config).await {
                Ok(hash) if hash == current_hash => {
                    tracing::debug!("published schema unchanged");
                }
                Ok(_) => match fetch_published_schema(&graph_ref, &platform_api_config).await {
                    Ok(published) => {
                        // Track the hash from the full fetch, not the poll: a
                        // publish can land between the two requests, and the
                        // stored hash must match the document actually emitted.
                        current_hash = published.hash;
                        if !send_update(&sender, published.document).await {
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::warn!("Failed to fetch published schema, will retry: {err}");
                    }
                },
                Err(err) => {
                    tracing::warn!("Failed to poll published schema, will retry: {err}");
                }
            }
        }
    });
    Box::pin(ReceiverStream::new(receiver))
}

/// Send a schema update to the stream, returning `false` if the receiver is gone.
async fn send_update(sender: &tokio::sync::mpsc::Sender<Event>, sdl: String) -> bool {
    sender
        .send(Event::UpdateSchema(SchemaState {
            sdl,
            launch_id: None,
        }))
        .await
        .inspect_err(|e| {
            tracing::debug!(
                "failed to push to schema stream. This is likely to be because the server is shutting down: {e}"
            );
        })
        .is_ok()
}

async fn fetch_published_schema(
    graph_ref: &str,
    platform_api_config: &PlatformApiConfig,
) -> Result<PublishedSchema, RequestError> {
    let response = graphql_request::<PublishedSchemaQuery>(
        &PublishedSchemaQuery::build_query(published_schema_query::Variables {
            graph_ref: graph_ref.to_string(),
        }),
        platform_api_config,
    )
    .await?;

    match response.variant {
        Some(published_schema_query::PublishedSchemaQueryVariant::GraphVariant(variant)) => variant
            .latest_publication
            .map(|publication| PublishedSchema {
                hash: publication.schema.hash,
                document: publication.schema.document,
            })
            .ok_or_else(|| RequestError::Response(format!("no schema published for {graph_ref}"))),
        Some(published_schema_query::PublishedSchemaQueryVariant::InvalidRefFormat(err)) => {
            Err(RequestError::Response(err.message))
        }
        None => Err(RequestError::Response(format!("{graph_ref} not found"))),
    }
}

async fn fetch_published_schema_hash(
    graph_ref: &str,
    platform_api_config: &PlatformApiConfig,
) -> Result<String, RequestError> {
    let response = graphql_request::<PublishedSchemaHashQuery>(
        &PublishedSchemaHashQuery::build_query(published_schema_hash_query::Variables {
            graph_ref: graph_ref.to_string(),
        }),
        platform_api_config,
    )
    .await?;

    match response.variant {
        Some(published_schema_hash_query::PublishedSchemaHashQueryVariant::GraphVariant(
            variant,
        )) => variant
            .latest_publication
            .map(|publication| publication.schema.hash)
            .ok_or_else(|| RequestError::Response(format!("no schema published for {graph_ref}"))),
        Some(published_schema_hash_query::PublishedSchemaHashQueryVariant::InvalidRefFormat(
            err,
        )) => Err(RequestError::Response(err.message)),
        None => Err(RequestError::Response(format!("{graph_ref} not found"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use secrecy::SecretString;
    use std::time::Duration;
    use tokio::time::timeout;
    use url::Url;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FULL_QUERY: &str = r#""operationName":"PublishedSchemaQuery""#;
    const HASH_QUERY: &str = r#""operationName":"PublishedSchemaHashQuery""#;

    fn schema_response(hash: &str, document: &str) -> String {
        serde_json::json!({
            "data": {
                "variant": {
                    "__typename": "GraphVariant",
                    "latestPublication": {
                        "schema": {
                            "hash": hash,
                            "document": document
                        }
                    }
                }
            }
        })
        .to_string()
    }

    fn hash_response(hash: &str) -> String {
        serde_json::json!({
            "data": {
                "variant": {
                    "__typename": "GraphVariant",
                    "latestPublication": {
                        "schema": {
                            "hash": hash
                        }
                    }
                }
            }
        })
        .to_string()
    }

    fn invalid_ref_response() -> String {
        serde_json::json!({
            "data": {
                "variant": {
                    "__typename": "InvalidRefFormat",
                    "message": "invalid graph ref"
                }
            }
        })
        .to_string()
    }

    fn no_publication_response() -> String {
        serde_json::json!({
            "data": {
                "variant": {
                    "__typename": "GraphVariant",
                    "latestPublication": null
                }
            }
        })
        .to_string()
    }

    fn platform_api_config(mock_server: &MockServer) -> PlatformApiConfig {
        PlatformApiConfig::new(
            SecretString::from("test-key"),
            Duration::from_millis(10),
            Duration::from_secs(5),
            Some(Url::parse(&mock_server.uri()).unwrap()),
        )
    }

    fn json_response(body: String) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_string(body)
    }

    async fn next_event(stream: &mut Pin<Box<dyn Stream<Item = Event> + Send>>) -> Event {
        timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("expected next schema event before timeout")
            .expect("expected schema stream to remain open")
    }

    fn assert_update_sdl(event: Event, expected_sdl: &str) {
        let Event::UpdateSchema(state) = event else {
            panic!("expected schema update, got {event:?}");
        };
        assert_eq!(state.sdl, expected_sdl);
    }

    #[tokio::test]
    async fn initial_load_succeeds() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(schema_response(
                "hash-1",
                "type Query { hello: String }",
            )))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut stream =
            stream_published_schema("graph@main".to_string(), platform_api_config(&mock_server));

        assert_update_sdl(
            next_event(&mut stream).await,
            "type Query { hello: String }",
        );
    }

    #[tokio::test]
    async fn hash_change_triggers_reload() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(schema_response(
                "hash-1",
                "type Query { hello: String }",
            )))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(HASH_QUERY))
            .respond_with(json_response(hash_response("hash-2")))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(schema_response(
                "hash-2",
                "type Query { hello: String, world: String }",
            )))
            .mount(&mock_server)
            .await;

        let mut stream =
            stream_published_schema("graph@main".to_string(), platform_api_config(&mock_server));

        assert_update_sdl(
            next_event(&mut stream).await,
            "type Query { hello: String }",
        );
        assert_update_sdl(
            next_event(&mut stream).await,
            "type Query { hello: String, world: String }",
        );
    }

    #[tokio::test]
    async fn unchanged_hash_emits_no_update() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(schema_response(
                "hash-1",
                "type Query { hello: String }",
            )))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(HASH_QUERY))
            .respond_with(json_response(hash_response("hash-1")))
            .mount(&mock_server)
            .await;

        let mut stream =
            stream_published_schema("graph@main".to_string(), platform_api_config(&mock_server));

        assert_update_sdl(
            next_event(&mut stream).await,
            "type Query { hello: String }",
        );

        // Several poll intervals pass with an unchanged hash; no update is emitted
        assert!(
            timeout(Duration::from_millis(100), stream.next())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn polling_continues_after_transient_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(schema_response(
                "hash-1",
                "type Query { hello: String }",
            )))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(HASH_QUERY))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(HASH_QUERY))
            .respond_with(json_response(hash_response("hash-2")))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(schema_response(
                "hash-2",
                "type Query { hello: String, world: String }",
            )))
            .mount(&mock_server)
            .await;

        let mut stream =
            stream_published_schema("graph@main".to_string(), platform_api_config(&mock_server));

        assert_update_sdl(
            next_event(&mut stream).await,
            "type Query { hello: String }",
        );
        assert_update_sdl(
            next_event(&mut stream).await,
            "type Query { hello: String, world: String }",
        );
    }

    #[tokio::test]
    async fn transient_error_at_startup_retries() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(schema_response(
                "hash-1",
                "type Query { hello: String }",
            )))
            .mount(&mock_server)
            .await;

        let mut stream =
            stream_published_schema("graph@main".to_string(), platform_api_config(&mock_server));

        assert_update_sdl(
            next_event(&mut stream).await,
            "type Query { hello: String }",
        );
    }

    #[tokio::test]
    async fn invalid_ref_ends_stream() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(invalid_ref_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut stream =
            stream_published_schema("bad ref".to_string(), platform_api_config(&mock_server));

        let event = timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("expected stream to end before timeout");
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn no_publication_ends_stream() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains(FULL_QUERY))
            .respond_with(json_response(no_publication_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut stream =
            stream_published_schema("graph@main".to_string(), platform_api_config(&mock_server));

        let event = timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("expected stream to end before timeout");
        assert!(event.is_none());
    }
}
