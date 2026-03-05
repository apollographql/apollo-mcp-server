use http::request::Parts;
use rhai::{CustomType, Engine, TypeBuilder};

use super::RhaiHeaderMap;

#[derive(Clone, Debug, CustomType)]
pub struct RhaiHttpParts {
    method: String,
    uri: String,
    headers: RhaiHeaderMap,
}

impl From<Parts> for RhaiHttpParts {
    fn from(parts: Parts) -> Self {
        Self {
            method: parts.method.to_string(),
            uri: parts.uri.to_string(),
            headers: RhaiHeaderMap::from(parts.headers),
        }
    }
}

impl RhaiHttpParts {
    pub fn register(engine: &mut Engine) {
        engine
            .register_type::<RhaiHttpParts>()
            .register_get("method", RhaiHttpParts::get_method)
            .register_get("uri", RhaiHttpParts::get_uri)
            .register_get("headers", RhaiHttpParts::get_headers);
    }

    fn get_method(&mut self) -> String {
        self.method.clone()
    }

    fn get_uri(&mut self) -> String {
        self.uri.clone()
    }

    fn get_headers(&mut self) -> RhaiHeaderMap {
        self.headers.clone()
    }
}
