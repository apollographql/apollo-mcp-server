use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};

use crate::platform_api::RequestError;

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

impl From<RequestError> for CollectionError {
    fn from(error: RequestError) -> Self {
        match error {
            RequestError::HeaderValue(e) => CollectionError::HeaderValue(e),
            RequestError::Request(e) => CollectionError::Request(e),
            RequestError::Response(message) => CollectionError::Response(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_request_error_maps_response_message() {
        let error = CollectionError::from(RequestError::Response("missing data".to_string()));
        assert!(matches!(error, CollectionError::Response(message) if message == "missing data"));
    }

    #[test]
    fn from_request_error_maps_header_value() {
        let invalid_value = reqwest::header::HeaderValue::from_bytes(b"\0invalid").unwrap_err();
        let error = CollectionError::from(RequestError::HeaderValue(invalid_value));
        assert!(matches!(error, CollectionError::HeaderValue(_)));
    }

    #[tokio::test]
    async fn from_request_error_maps_request() {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;
        let reqwest_error = reqwest::get(mock_server.uri())
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err();

        let error = CollectionError::from(RequestError::Request(reqwest_error));
        assert!(matches!(error, CollectionError::Request(_)));
    }
}
