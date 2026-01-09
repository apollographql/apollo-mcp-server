//! WWW Authenticate header definition.
//!
//! TODO: This might be nice to upstream to hyper.

use headers::{Header, HeaderValue};
use http::header::WWW_AUTHENTICATE;
use tracing::warn;
use url::Url;

pub(super) enum WwwAuthenticate {
    Bearer {
        resource_metadata: Url,
        scope: Option<String>,
        error: Option<BearerError>,
    },
}

/// OAuth 2.0 Bearer Token error codes per RFC 6750 Section 3.1
#[derive(Debug, Clone)]
pub(super) enum BearerError {
    /// The request requires higher privileges than provided by the access token.
    InsufficientScope,
}

impl Header for WwwAuthenticate {
    fn name() -> &'static http::HeaderName {
        &WWW_AUTHENTICATE
    }

    fn decode<'i, I>(_values: &mut I) -> Result<Self, headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i http::HeaderValue>,
    {
        // We don't care about decoding, so we do nothing here.
        Err(headers::Error::invalid())
    }

    fn encode<E: Extend<http::HeaderValue>>(&self, values: &mut E) {
        let encoded = match &self {
            WwwAuthenticate::Bearer {
                resource_metadata,
                scope,
                error,
            } => {
                let mut header = format!(
                    r#"Bearer resource_metadata="{}""#,
                    resource_metadata.as_str()
                );
                // Error must come before scope per RFC 6750 examples
                if let Some(err) = error {
                    let error_str = match err {
                        BearerError::InsufficientScope => "insufficient_scope",
                    };
                    header.push_str(&format!(r#", error="{}""#, error_str));
                }
                if let Some(scope) = scope {
                    header.push_str(&format!(r#", scope="{}""#, scope));
                }
                header
            }
        };

        // TODO: This shouldn't error, but it can so we might need to do something else here
        match HeaderValue::from_str(&encoded) {
            Ok(value) => values.extend(std::iter::once(value)),
            Err(e) => warn!("could not construct WWW-AUTHENTICATE header: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use headers::Header;

    fn encode_header(header: &WwwAuthenticate) -> String {
        let mut values = Vec::new();
        header.encode(&mut values);
        values
            .first()
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn encode_bearer_without_scope() {
        let header = WwwAuthenticate::Bearer {
            resource_metadata: Url::parse("https://test.com/.well-known/oauth-protected-resource")
                .unwrap(),
            scope: None,
            error: None,
        };

        let encoded = encode_header(&header);
        assert_eq!(
            encoded,
            r#"Bearer resource_metadata="https://test.com/.well-known/oauth-protected-resource""#
        );
    }

    #[test]
    fn encode_bearer_with_single_scope() {
        let header = WwwAuthenticate::Bearer {
            resource_metadata: Url::parse(
                "https://mcp.test.com/.well-known/oauth-protected-resource",
            )
            .unwrap(),
            scope: Some("read".to_string()),
            error: None,
        };

        let encoded = encode_header(&header);
        assert!(encoded.contains("Bearer"));
        assert!(encoded.contains("resource_metadata="));
        assert!(encoded.contains(r#"scope="read""#));
    }

    #[test]
    fn encode_bearer_with_multiple_scopes() {
        let header = WwwAuthenticate::Bearer {
            resource_metadata: Url::parse("https://test.com/.well-known/oauth-protected-resource")
                .unwrap(),
            scope: Some("read write".to_string()),
            error: None,
        };

        let encoded = encode_header(&header);
        assert_eq!(
            encoded,
            r#"Bearer resource_metadata="https://test.com/.well-known/oauth-protected-resource", scope="read write""#
        );
    }

    #[test]
    fn encode_bearer_with_insufficient_scope_error() {
        let header = WwwAuthenticate::Bearer {
            resource_metadata: Url::parse("https://test.com/.well-known/oauth-protected-resource")
                .unwrap(),
            scope: Some("read write".to_string()),
            error: Some(BearerError::InsufficientScope),
        };

        let encoded = encode_header(&header);
        assert_eq!(
            encoded,
            r#"Bearer resource_metadata="https://test.com/.well-known/oauth-protected-resource", error="insufficient_scope", scope="read write""#
        );
    }
}
