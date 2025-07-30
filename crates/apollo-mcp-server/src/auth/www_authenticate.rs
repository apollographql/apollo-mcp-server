//! WWW Authenticate header definition.
//!
//! TODO: This might be nice to upstream to hyper.

use headers::{Header, HeaderValue};
use http::header::WWW_AUTHENTICATE;
use tracing::warn;
use url::Url;

pub(super) enum WwwAuthenticate {
    Bearer { resource_metadata: Url },
}

impl Header for WwwAuthenticate {
    fn name() -> &'static http::HeaderName {
        &WWW_AUTHENTICATE
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i http::HeaderValue>,
    {
        // We don't care about decoding, so we do nothing here.
        Err(headers::Error::invalid())
    }

    fn encode<E: Extend<http::HeaderValue>>(&self, values: &mut E) {
        let encoded = match &self {
            WwwAuthenticate::Bearer { resource_metadata } => format!(
                r#"Bearer resource_metadata="{}""#,
                resource_metadata.as_str()
            ),
        };

        // TODO: This shouldn't error, but it can so we might need to do something else here
        match HeaderValue::from_str(&encoded) {
            Ok(value) => values.extend(std::iter::once(value)),
            Err(e) => warn!("could not construct WWW-AUTHENTICATE header: {e}"),
        }
    }
}
