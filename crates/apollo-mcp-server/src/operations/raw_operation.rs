use std::{collections::HashMap, str::FromStr as _};

use apollo_compiler::validation::Valid;
use apollo_mcp_registry::platform_api::operation_collections::{
    collection_poller::OperationData, error::CollectionError,
};
use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::{custom_scalar_map::CustomScalarMap, errors::OperationError};

use super::{MutationMode, operation::Operation};

#[derive(Debug, Clone)]
pub struct RawOperation {
    pub(crate) source_text: String,
    pub(super) persisted_query_id: Option<String>,
    pub(super) headers: Option<HeaderMap<HeaderValue>>,
    pub(crate) variables: Option<HashMap<String, Value>>,
    pub(super) source_path: Option<String>,
}

impl RawOperation {
    pub(crate) fn into_operation(
        self,
        schema: &Valid<apollo_compiler::Schema>,
        custom_scalars: Option<&CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
        enable_output_schema: bool,
    ) -> Result<Option<Operation>, OperationError> {
        Operation::from_document(
            self,
            schema,
            custom_scalars,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
            enable_output_schema,
        )
    }
}

impl From<(String, Option<String>)> for RawOperation {
    fn from((source_text, source_path): (String, Option<String>)) -> Self {
        Self {
            persisted_query_id: None,
            source_text,
            headers: None,
            variables: None,
            source_path,
        }
    }
}

impl From<(String, String)> for RawOperation {
    fn from((persisted_query_id, source_text): (String, String)) -> Self {
        Self {
            persisted_query_id: Some(persisted_query_id),
            source_text,
            headers: None,
            variables: None,
            source_path: None,
        }
    }
}

impl TryFrom<&OperationData> for RawOperation {
    type Error = CollectionError;

    fn try_from(operation_data: &OperationData) -> Result<Self, Self::Error> {
        let variables = if let Some(variables) = operation_data.variables.as_ref() {
            if variables.trim().is_empty() {
                Some(HashMap::new())
            } else {
                Some(
                    serde_json::from_str::<HashMap<String, Value>>(variables)
                        .map_err(|_| CollectionError::InvalidVariables(variables.clone()))?,
                )
            }
        } else {
            None
        };

        let headers = if let Some(headers) = operation_data.headers.as_ref() {
            let mut header_map = HeaderMap::new();
            for header in headers {
                header_map.insert(
                    HeaderName::from_str(&header.0).map_err(CollectionError::HeaderName)?,
                    HeaderValue::from_str(&header.1).map_err(CollectionError::HeaderValue)?,
                );
            }
            Some(header_map)
        } else {
            None
        };

        Ok(Self {
            persisted_query_id: None,
            source_text: operation_data.source_text.clone(),
            headers,
            variables,
            source_path: None,
        })
    }
}

// TODO: This can be greatly simplified by using `serde::serialize_with` on the specific field that does not
// implement `Serialize`.
// Custom Serialize implementation for RawOperation
// This is needed because reqwest HeaderMap/HeaderValue/HeaderName don't derive Serialize
impl serde::Serialize for RawOperation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("RawOperation", 4)?;
        state.serialize_field("source_text", &self.source_text)?;
        if let Some(ref id) = self.persisted_query_id {
            state.serialize_field("persisted_query_id", id)?;
        }
        if let Some(ref variables) = self.variables {
            state.serialize_field("variables", variables)?;
        }
        if let Some(ref headers) = self.headers {
            state.serialize_field(
                "headers",
                headers
                    .iter()
                    .map(|(name, value)| {
                        format!("{}: {}", name, value.to_str().unwrap_or_default())
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    .as_str(),
            )?;
        }
        if let Some(ref path) = self.source_path {
            state.serialize_field("source_path", path)?;
        }

        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_data(source_text: &str, variables: Option<&str>) -> OperationData {
        OperationData {
            id: String::new(),
            last_updated_at: String::new(),
            source_text: source_text.to_string(),
            variables: variables.map(|v| v.to_string()),
            headers: None,
        }
    }

    #[test]
    fn try_from_fails_with_malformed_variables_json() {
        let data = operation_data(
            "query GetUser($id: ID!) { user(id: $id) { name } }",
            Some(r#"{id: "123"}"#), // unquoted key
        );
        let result = RawOperation::try_from(&data);
        assert!(
            matches!(result, Err(CollectionError::InvalidVariables(_))),
            "expected InvalidVariables error for malformed JSON, got {result:?}"
        );
    }

    #[test]
    fn one_bad_operation_does_not_poison_batch() {
        let operations = [
            operation_data(
                "query GetUser($id: ID!) { user(id: $id) { name } }",
                Some(r#"{"id": "123"}"#),
            ),
            operation_data("query ListUsers { users { name } }", None),
            operation_data(
                "query SearchUsers($term: String!) { search(term: $term) { name } }",
                Some(r#"not valid json"#),
            ),
        ];

        let recovered: Vec<RawOperation> = operations
            .iter()
            .filter_map(|op| RawOperation::try_from(op).ok())
            .collect();

        assert_eq!(recovered.len(), 2);
    }
}
