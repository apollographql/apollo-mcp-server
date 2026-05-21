use std::sync::Arc;

use reqwest::header::HeaderMap;

/// A v2 seam: produces the upstream headers to use for a given (graph, user)
/// combination. v1 always returns `base` unchanged.
pub trait CredentialProvider: Send + Sync + std::fmt::Debug {
    fn headers_for(&self, base: &HeaderMap, user: Option<&str>) -> HeaderMap;
}

/// Default v1 implementation: returns the base headers untouched.
#[derive(Debug, Default)]
pub struct PassthroughCredentials;

impl CredentialProvider for PassthroughCredentials {
    fn headers_for(&self, base: &HeaderMap, _user: Option<&str>) -> HeaderMap {
        base.clone()
    }
}

pub fn default_provider() -> Arc<dyn CredentialProvider> {
    Arc::new(PassthroughCredentials)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{AUTHORIZATION, HeaderValue};

    #[test]
    fn passthrough_returns_base_unchanged() {
        let mut base = HeaderMap::new();
        base.insert(AUTHORIZATION, HeaderValue::from_static("Bearer x"));
        let p = PassthroughCredentials;
        let got = p.headers_for(&base, None);
        assert_eq!(got.get(AUTHORIZATION).unwrap(), "Bearer x");
    }
}
