use crate::errors::ServerError;
use rmcp::serde_json;
use schemars::Schema;
use std::{collections::HashMap, path::PathBuf, str::FromStr};

impl FromStr for CustomScalarMap {
    type Err = ServerError;

    fn from_str(string_custom_scalar_file: &str) -> Result<Self, Self::Err> {
        // Parse the string into an initial map of serde_json::Values
        let parsed_custom_scalar_file: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(string_custom_scalar_file)
                .map_err(ServerError::CustomScalarConfig)?;

        // Try to parse each as a schema
        let custom_scalar_map = parsed_custom_scalar_file
            .into_iter()
            .map(|(key, value)| {
                // The schemars crate does not enforce schema validation anymore, so we use jsonschema
                // to ensure that the supplied schema is valid.
                if let Err(e) = jsonschema::meta::validate(&value) {
                    return Err(ServerError::CustomScalarJsonSchema(e.to_string()));
                }

                Schema::try_from(value.clone())
                    .map(|schema| (key, schema))
                    .map_err(|e| ServerError::CustomScalarJsonSchema(e.to_string()))
            })
            .collect::<Result<_, _>>()?;

        Ok(CustomScalarMap(custom_scalar_map))
    }
}

impl TryFrom<&PathBuf> for CustomScalarMap {
    type Error = ServerError;

    fn try_from(file_path_buf: &PathBuf) -> Result<Self, Self::Error> {
        let custom_scalars_config_path = file_path_buf.as_path();
        tracing::debug!(custom_scalars_config=?custom_scalars_config_path, "Loading custom_scalars_config");
        let string_custom_scalar_file = std::fs::read_to_string(custom_scalars_config_path)?;
        CustomScalarMap::from_str(string_custom_scalar_file.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct CustomScalarMap(HashMap<String, Schema>);

impl CustomScalarMap {
    pub fn get(&self, key: &str) -> Option<&Schema> {
        self.0.get(key)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr};

    use schemars::json_schema;

    use crate::custom_scalar_map::CustomScalarMap;

    #[test]
    fn empty_file() {
        let result = CustomScalarMap::from_str("").err().unwrap();

        insta::assert_debug_snapshot!(result, @r#"
        CustomScalarConfig(
            Error("EOF while parsing a value", line: 1, column: 0),
        )
        "#)
    }

    #[test]
    fn only_spaces() {
        let result =
            CustomScalarMap::from_str("    ").expect_err("empty space should be valid schema");

        insta::assert_debug_snapshot!(result, @r#"
        CustomScalarConfig(
            Error("EOF while parsing a value", line: 1, column: 4),
        )
        "#)
    }

    #[test]
    fn invalid_json() {
        let result = CustomScalarMap::from_str("Hello: }").err().unwrap();

        insta::assert_debug_snapshot!(result, @r#"
        CustomScalarConfig(
            Error("expected value", line: 1, column: 1),
        )
        "#)
    }

    #[test]
    fn invalid_simple_schema() {
        let result = CustomScalarMap::from_str(
            r###"{
                "custom": {
                    "type": "bool"
                }
            }"###,
        )
        .expect_err("schema should have been invalid");

        insta::assert_debug_snapshot!(result, @r###"
        CustomScalarJsonSchema(
            "\"bool\" is not valid under any of the schemas listed in the 'anyOf' keyword",
        )
        "###)
    }

    #[test]
    fn invalid_complex_schema() {
        let result = CustomScalarMap::from_str(
            r###"{
                "custom": {
                    "type": "object",
                    "properties": {
                        "test": {
                            "type": "obbbject"
                        }
                    }
                }
            }"###,
        )
        .expect_err("schema should have been invalid");

        insta::assert_debug_snapshot!(result, @r#"
        CustomScalarJsonSchema(
            "\"obbbject\" is not valid under any of the schemas listed in the 'anyOf' keyword",
        )
        "#)
    }

    #[test]
    fn valid_schema() {
        let result = CustomScalarMap::from_str(
            r###"
        {
            "simple": {
                "type": "string"
            },
            "complex": {
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }
        }
        "###,
        )
        .unwrap()
        .0;

        let expected_data = HashMap::from_iter([
            (
                "simple".to_string(),
                json_schema!({
                    "type": "string",
                })
                .try_into()
                .unwrap(),
            ),
            (
                "complex".to_string(),
                json_schema!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string"
                        }
                    }
                }),
            ),
        ]);

        assert_eq!(result, expected_data);
    }
}
