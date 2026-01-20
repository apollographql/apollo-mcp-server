use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use tokio::sync::mpsc::Sender;

use super::event::CollectionEvent;

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    #[error(transparent)]
    HeaderName(InvalidHeaderName),

    #[error(transparent)]
    HeaderValue(InvalidHeaderValue),

    #[error(transparent)]
    Request(reqwest::Error),

    #[error("Error in response: {0}")]
    Response(String),

    #[error("invalid variables: {0}")]
    InvalidVariables(String),
}

impl CollectionError {
    /// Returns `true` if the error is permanent and sends it to the collection stream.
    /// Returns `false` if the error is transient (will be retried).
    #[allow(clippy::wrong_self_convention)] // Intentionally consumes self to send over channel
    pub(super) async fn is_permanent(self, sender: &Sender<CollectionEvent>) -> bool {
        // Check if the underlying reqwest error is transient
        if matches!(&self, CollectionError::Request(req_err) if
            req_err.is_connect()
            || req_err.is_timeout()
            || req_err.is_request()
            || req_err.status().is_some_and(|status| {
                status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            })
        ) {
            tracing::warn!(
                "Failed to fetch operation collection with transient error, will retry: {self}"
            );
            false
        } else {
            tracing::error!("Failed to fetch operation collection with permanent error: {self}");
            if let Err(e) = sender.send(CollectionEvent::CollectionError(self)).await {
                tracing::debug!(
                    "Failed to send error to collection stream. This is likely to be because the server is shutting down: {e}"
                );
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::channel;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_response_error_is_permanent() {
        let (sender, _receiver) = channel(1);
        let error = CollectionError::Response("permission denied".to_string());
        assert!(error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_header_name_error_is_permanent() {
        let (sender, _receiver) = channel(1);
        let invalid_name = reqwest::header::HeaderName::from_bytes(b"\0invalid").unwrap_err();
        let error = CollectionError::HeaderName(invalid_name);
        assert!(error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_header_value_error_is_permanent() {
        let (sender, _receiver) = channel(1);
        let invalid_value = reqwest::header::HeaderValue::from_bytes(b"\0invalid").unwrap_err();
        let error = CollectionError::HeaderValue(invalid_value);
        assert!(error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_invalid_variables_error_is_permanent() {
        let (sender, _receiver) = channel(1);
        let error = CollectionError::InvalidVariables("bad json".to_string());
        assert!(error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_client_error_404_is_permanent() {
        let (sender, _receiver) = channel(1);

        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let result = reqwest::get(mock_server.uri()).await.unwrap();
        let reqwest_error = result.error_for_status().unwrap_err();

        let error = CollectionError::Request(reqwest_error);
        assert!(error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_permanent_error_with_closed_channel() {
        let (sender, receiver) = channel(1);
        drop(receiver);

        let error = CollectionError::Response("permission denied".to_string());
        assert!(error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_connection_error_is_not_permanent() {
        let (sender, _receiver) = channel(1);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(1))
            .build()
            .unwrap();

        let result = client.get("http://192.0.2.1:1").send().await;
        let reqwest_error = result.unwrap_err();

        let error = CollectionError::Request(reqwest_error);
        assert!(!error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_timeout_error_is_not_permanent() {
        let (sender, _receiver) = channel(1);

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

        let error = CollectionError::Request(reqwest_error);
        assert!(!error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_server_error_is_not_permanent() {
        let (sender, _receiver) = channel(1);

        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let result = reqwest::get(mock_server.uri()).await.unwrap();
        let reqwest_error = result.error_for_status().unwrap_err();

        let error = CollectionError::Request(reqwest_error);
        assert!(!error.is_permanent(&sender).await);
    }

    #[tokio::test]
    async fn test_rate_limit_429_is_not_permanent() {
        let (sender, _receiver) = channel(1);

        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let result = reqwest::get(mock_server.uri()).await.unwrap();
        let reqwest_error = result.error_for_status().unwrap_err();

        let error = CollectionError::Request(reqwest_error);
        assert!(!error.is_permanent(&sender).await);
    }
}
