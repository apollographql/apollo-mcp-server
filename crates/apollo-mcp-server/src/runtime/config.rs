use std::path::PathBuf;

use apollo_mcp_server::server::Transport;
use reqwest::header::HeaderMap;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::Level;
use url::Url;

use super::{OperationSource, SchemaSource, graphos::GraphOSCredentials, overrides::Overrides};

/// Configuration for the MCP server
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Config {
    /// Apollo-specific credential overrides
    #[serde(default)]
    pub graphos: GraphOSCredentials,

    /// Path to a custom scalar map
    pub custom_scalars: Option<PathBuf>,

    /// The target GraphQL endpoint
    pub endpoint: Url,

    /// List of hard-coded headers to include in all GraphQL requests
    #[serde(default, deserialize_with = "parsers::map_from_str")]
    #[schemars(schema_with = "super::schemas::header_map")]
    pub headers: HeaderMap,

    /// Operations
    pub operations: OperationSource,

    /// Overrides for server behaviour
    #[serde(default)]
    pub overrides: Overrides,

    /// The log level to use for tracing
    #[serde(
        default = "defaults::log_level",
        deserialize_with = "parsers::from_str"
    )]
    #[schemars(schema_with = "super::schemas::level")]
    pub log_level: Level,

    /// The schema to load for operations
    pub schema: SchemaSource,

    /// The type of server transport to use
    #[serde(default)]
    pub transport: Transport,
}

mod defaults {
    use tracing::Level;

    pub(super) fn log_level() -> Level {
        Level::INFO
    }
}

mod parsers {
    use std::{fmt::Display, marker::PhantomData, str::FromStr};

    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    use serde::Deserializer;

    pub(super) fn from_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: FromStr,
        <T as FromStr>::Err: Display,
    {
        struct FromStrVisitor<Inner> {
            _phantom: PhantomData<Inner>,
        }
        impl<Inner> serde::de::Visitor<'_> for FromStrVisitor<Inner>
        where
            Inner: FromStr,
            <Inner as FromStr>::Err: Display,
        {
            type Value = Inner;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Inner::from_str(v).map_err(|e| serde::de::Error::custom(e.to_string()))
            }
        }

        deserializer.deserialize_str(FromStrVisitor {
            _phantom: PhantomData,
        })
    }

    pub(super) fn map_from_str<'de, D>(deserializer: D) -> Result<HeaderMap, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MapFromStrVisitor;
        impl<'de> serde::de::Visitor<'de> for MapFromStrVisitor {
            type Value = HeaderMap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a map of header string keys and values")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut parsed = HeaderMap::with_capacity(map.size_hint().unwrap_or(0));

                // While there are entries remaining in the input, add them
                // into our map.
                while let Some((key, value)) = map.next_entry()? {
                    let key = HeaderName::from_str(key)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))?;
                    let value = HeaderValue::from_str(value)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))?;

                    parsed.insert(key, value);
                }

                Ok(parsed)
            }
        }

        deserializer.deserialize_str(MapFromStrVisitor)
    }
}
