use http::header::{HeaderName, InvalidHeaderName, InvalidHeaderValue};
use http::{HeaderMap, HeaderValue};
use rhai::{CustomType, Engine, EvalAltResult, Position, TypeBuilder};

#[derive(Clone, Debug, CustomType)]
pub(crate) struct RhaiHeaderMap {
    header_map: HeaderMap,
}

impl From<HeaderMap> for RhaiHeaderMap {
    fn from(header_map: HeaderMap) -> Self {
        Self { header_map }
    }
}

impl RhaiHeaderMap {
    fn get_field(&mut self, key: String) -> String {
        self.header_map
            .get(key.as_str())
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    fn set_field(&mut self, key: String, value: String) -> Result<(), Box<EvalAltResult>> {
        let header_value = HeaderValue::from_str(&value).map_err(|e: InvalidHeaderValue| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("invalid header value: {e}").into(),
                Position::NONE,
            ))
        })?;
        let header_name =
            HeaderName::from_bytes(key.as_bytes()).map_err(|e: InvalidHeaderName| {
                Box::new(EvalAltResult::ErrorRuntime(
                    format!("invalid header name: {e}").into(),
                    Position::NONE,
                ))
            })?;
        self.header_map.insert(header_name, header_value);
        Ok(())
    }

    pub(crate) fn register(engine: &mut Engine) {
        engine
            .register_type::<RhaiHeaderMap>()
            .register_indexer_get(RhaiHeaderMap::get_field)
            .register_indexer_set(RhaiHeaderMap::set_field);
    }

    pub(crate) fn as_header_map(&self) -> HeaderMap {
        self.header_map.clone()
    }
}
