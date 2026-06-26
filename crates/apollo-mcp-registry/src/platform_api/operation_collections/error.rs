use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};

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
    /// Returns `true` if the error is transient according to the Platform API fetch policy.
    pub(super) fn is_transient(&self) -> bool {
        matches!(self, CollectionError::Request(req_err) if
            req_err.is_connect()
            || req_err.is_timeout()
            || req_err.is_request()
            || req_err.status().is_some_and(|status| {
                status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn response_error_is_not_transient() {
        let error = CollectionError::Response("permission denied".to_string());
        assert!(!error.is_transient());
    }

    #[test]
    fn header_name_error_is_not_transient() {
        let invalid_name = reqwest::header::HeaderName::from_bytes(b"\0invalid").unwrap_err();
        let error = CollectionError::HeaderName(invalid_name);
        assert!(!error.is_transient());
    }

    #[test]
    fn header_value_error_is_not_transient() {
        let invalid_value = reqwest::header::HeaderValue::from_bytes(b"\0invalid").unwrap_err();
        let error = CollectionError::HeaderValue(invalid_value);
        assert!(!error.is_transient());
    }

    #[test]
    fn invalid_variables_error_is_not_transient() {
        let error = CollectionError::InvalidVariables("bad json".to_string());
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

        let error = CollectionError::Request(reqwest_error);
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

        let error = CollectionError::Request(reqwest_error);
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

        let error = CollectionError::Request(reqwest_error);
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

        let error = CollectionError::Request(reqwest_error);
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

        let error = CollectionError::Request(reqwest_error);
        assert!(error.is_transient());
    }
}
