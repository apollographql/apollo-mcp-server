use std::path::PathBuf;

use apollo_mcp_server::schema_validation::SchemaValidation;
use schemars::JsonSchema;
use serde::Deserialize;

/// Upstream GraphQL schema configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SchemaConfig {
    /// Where to load the schema from
    #[serde(flatten)]
    pub source: SchemaSource,

    /// How strictly to validate a schema that isn't a supergraph
    #[serde(default)]
    pub validation: SchemaValidation,
}

/// Source for upstream GraphQL schema
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum SchemaSource {
    /// Schema should be loaded (and watched) from a local file path
    Local { path: PathBuf },

    /// Fetch the schema from uplink
    #[default]
    Uplink,

    /// Fetch the latest published schema from the GraphOS Platform API.
    /// Unlike uplink, this works for non-federated graphs.
    Graphos,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_graphos_source() {
        let source: SchemaSource = serde_yaml::from_str("source: graphos").unwrap();
        assert!(matches!(source, SchemaSource::Graphos));
    }

    #[rstest::rstest]
    #[case::omitted("source: uplink", SchemaValidation::Strict)]
    #[case::strict("source: uplink\nvalidation: strict", SchemaValidation::Strict)]
    #[case::lenient("source: uplink\nvalidation: lenient", SchemaValidation::Lenient)]
    fn deserialize_validation(#[case] yaml: &str, #[case] expected: SchemaValidation) {
        let config: SchemaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.validation, expected);
    }

    #[test]
    fn deserialize_validation_alongside_source_fields() {
        let config: SchemaConfig =
            serde_yaml::from_str("source: local\npath: schema.graphql\nvalidation: lenient")
                .unwrap();

        assert!(
            matches!(&config.source, SchemaSource::Local { path } if path == &PathBuf::from("schema.graphql"))
        );
        assert_eq!(config.validation, SchemaValidation::Lenient);
    }

    #[test]
    fn default_validation_is_strict() {
        assert_eq!(SchemaConfig::default().validation, SchemaValidation::Strict);
    }

    #[rstest::rstest]
    #[case::unknown_value("source: uplink\nvalidation: loose")]
    #[case::unknown_field("source: local\npath: schema.graphql\nunknown: true")]
    fn rejects_invalid_schema_config(#[case] yaml: &str) {
        assert!(serde_yaml::from_str::<SchemaConfig>(yaml).is_err());
    }
}
