//! Endpoint newtype
//!
//! This module defines a simple newtype around a Url for demarking a GraphQL
//! endpoint. This allows overlaying validation and default behaviour on top
//! of the wrapped URL.

use std::ops::Deref;

use serde::Deserialize;
use url::Url;

/// A GraphQL endpoint
#[derive(Debug)]
pub struct Endpoint(Url);

impl Endpoint {
    /// Unwrap the endpoint into its inner URL
    pub fn into_inner(self) -> Url {
        self.0
    }
}

impl Default for Endpoint {
    fn default() -> Self {
        Self(defaults::endpoint())
    }
}

impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // This is a simple wrapper around URL, so we just use its deserializer
        let url = Url::deserialize(deserializer)?;
        Ok(Self(url))
    }
}

impl Deref for Endpoint {
    type Target = Url;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

mod defaults {
    use url::Url;

    pub(super) fn endpoint() -> Url {
        // SAFETY: This should always parse correctly and is considered a breaking
        // error otherwise. It is also explicitly tested in [test::default_endpoint_parses_correctly]
        #[allow(clippy::unwrap_used)]
        Url::parse("http://127.0.0.1:4000").unwrap()
    }

    #[cfg(test)]
    mod test {
        use super::endpoint;

        #[test]
        fn default_endpoint_parses_correctly() {
            endpoint();
        }
    }
}
