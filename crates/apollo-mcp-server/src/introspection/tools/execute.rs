use crate::operations::private_fields::process_private_directives;
use crate::operations::{MutationMode, operation_defs, operation_name};
use crate::{
    graphql::{self, OperationDetails, ValidationError},
    schema_from_type,
};
use reqwest::header::{HeaderMap, HeaderValue};
use rmcp::model::{Tool, ToolAnnotations};
use rmcp::schemars::JsonSchema;
use rmcp::serde_json::Value;
use rmcp::{schemars, serde_json};
use serde::Deserialize;

use super::description::append_description_hint;

/// The name of the tool to execute an ad hoc GraphQL operation
pub const EXECUTE_TOOL_NAME: &str = "execute";

/// Executes an ad hoc GraphQL operation against the configured endpoint.
///
/// The tool parses one operation and enforces subscription and mutation-mode restrictions, but it
/// does not validate the document against the configured schema. Schema validation is a separate
/// `validate` tool call or an upstream GraphQL endpoint responsibility.
#[derive(Clone)]
pub struct Execute {
    pub tool: Tool,
    mutation_mode: MutationMode,
}

/// Input for the execute tool.
#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The GraphQL operation
    query: String,

    /// The variable values represented as JSON
    #[schemars(schema_with = "String::json_schema", default)]
    variables: Option<Value>,
}

impl Execute {
    pub fn new(mutation_mode: MutationMode, description_hint: Option<&str>) -> Self {
        let description = append_description_hint(
            "Execute a GraphQL operation. Use the `introspect` tool to get information about the GraphQL schema. Always use the schema to create operations - do not try arbitrary operations. If available, first use the `validate` tool to validate operations. DO NOT try to execute introspection queries.",
            description_hint,
        );
        // Ad hoc mutations are only allowed in MutationMode::All.
        let permits_mutation = mutation_mode == MutationMode::All;
        let mut annotations = ToolAnnotations::new()
            .read_only(!permits_mutation)
            .destructive(permits_mutation);
        annotations.idempotent_hint = Some(!permits_mutation);
        annotations.open_world_hint = Some(true);

        Self {
            mutation_mode,
            tool: Tool::new(EXECUTE_TOOL_NAME, description, schema_from_type!(Input))
                .annotate(annotations),
        }
    }
}

impl graphql::Executable for Execute {
    fn operation(&self, input: Value) -> Result<OperationDetails, ValidationError> {
        let input = serde_json::from_value::<Input>(input)
            .map_err(|e| ValidationError(format!("Invalid input: {e}")))?;

        let (_, operation_def, source_path) =
            operation_defs(&input.query, self.mutation_mode == MutationMode::All, None)
                .map_err(|e| ValidationError(e.to_string()))?
                .ok_or_else(|| ValidationError("Invalid operation type".into()))?;

        let op_name = operation_name(&operation_def, source_path).ok();

        // Check for @private directives and strip them from the query sent downstream
        let (query, private_fields) = match process_private_directives(&input.query) {
            Some((stripped_query, tree)) => (stripped_query, Some(tree)),
            None => (input.query, None),
        };

        Ok(OperationDetails {
            query,
            operation_name: op_name,
            private_fields,
        })
    }

    fn variables(&self, input: Value) -> Result<Value, ValidationError> {
        let input = serde_json::from_value::<Input>(input)
            .map_err(|e| ValidationError(format!("Invalid input: {e}")))?;
        match input.variables {
            None => Ok(Value::Null),
            Some(Value::Null) => Ok(Value::Null),
            Some(Value::String(s)) => serde_json::from_str(&s)
                .map_err(|e| ValidationError(format!("Invalid variables: {e}"))),
            Some(obj) if obj.is_object() => Ok(obj),
            _ => Err(ValidationError(
                "Variables must be a JSON object or string".into(),
            )),
        }
    }

    fn headers(&self, default_headers: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue> {
        default_headers.clone()
    }
}

#[cfg(test)]
mod tests {
    use crate::graphql::{Executable, OperationDetails, ValidationError};
    use crate::introspection::tools::execute::Execute;
    use crate::operations::MutationMode;
    use rmcp::serde_json::{Value, json};

    #[test]
    fn execute_query_with_variables_as_string() {
        let execute = Execute::new(MutationMode::None, None);

        let query = "query GetUser($id: ID!) { user(id: $id) { id name } }";
        let variables = json!({ "id": "123" });

        let input = json!({
            "query": query,
            "variables": variables.to_string()
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Ok(OperationDetails {
                query: query.to_string(),
                operation_name: Some("GetUser".to_string()),
                private_fields: None,
            })
        );
        assert_eq!(Executable::variables(&execute, input), Ok(variables));
    }

    #[test]
    fn execute_query_with_variables_as_json() {
        let execute = Execute::new(MutationMode::None, None);

        let query = "query GetUser($id: ID!) { user(id: $id) { id name } }";
        let variables = json!({ "id": "123" });

        let input = json!({
            "query": query,
            "variables": variables
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Ok(OperationDetails {
                query: query.to_string(),
                operation_name: Some("GetUser".to_string()),
                private_fields: None,
            })
        );
        assert_eq!(Executable::variables(&execute, input), Ok(variables));
    }

    #[test]
    fn execute_query_without_variables() {
        let execute = Execute::new(MutationMode::None, None);

        let query = "query GetUser($id: ID!) { user(id: $id) { id name } }";

        let input = json!({
            "query": query,
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Ok(OperationDetails {
                query: query.to_string(),
                operation_name: Some("GetUser".to_string()),
                private_fields: None,
            })
        );
        assert_eq!(Executable::variables(&execute, input), Ok(Value::Null));
    }

    #[test]
    fn execute_query_anonymous_operation() {
        let execute = Execute::new(MutationMode::None, None);

        let query = "{ user(id: \"123\") { id name } }";
        let input = json!({
            "query": query,
        });

        assert_eq!(
            Executable::operation(&execute, input.clone()),
            Ok(OperationDetails {
                query: query.to_string(),
                operation_name: None,
                private_fields: None,
            })
        );
    }

    #[test]
    fn execute_query_err_with_mutation_when_mutation_mode_is_none() {
        let execute = Execute::new(MutationMode::None, None);

        let query = "mutation MutationName { id }".to_string();
        let input = json!({
            "query": query,
        });

        let result = Executable::operation(&execute, input);
        assert!(matches!(result, Err(ValidationError(msg)) if msg == "Invalid operation type"));
    }

    #[test]
    fn execute_query_ok_with_mutation_when_mutation_mode_is_all() {
        let execute = Execute::new(MutationMode::All, None);

        let query = "mutation MutationName { id }".to_string();
        let input = json!({
            "query": query,
        });

        assert_eq!(
            Executable::operation(&execute, input),
            Ok(OperationDetails {
                query: query.to_string(),
                operation_name: Some("MutationName".to_string()),
                private_fields: None,
            })
        );
    }

    #[test]
    fn execute_query_err_with_subscription_regardless_of_mutation_mode() {
        for mutation_mode in [
            MutationMode::None,
            MutationMode::Explicit,
            MutationMode::All,
        ] {
            let execute = Execute::new(mutation_mode, None);

            let input = json!({
                "query": "subscription SubscriptionName { id }",
            });

            let result = Executable::operation(&execute, input);
            assert!(matches!(result, Err(ValidationError(msg)) if msg == "Invalid operation type"));
        }
    }

    #[test]
    fn execute_query_invalid_input() {
        let execute = Execute::new(MutationMode::None, None);

        let input = json!({
            "nonsense": "whatever",
        });

        let op_result = Executable::operation(&execute, input.clone());
        assert!(matches!(op_result, Err(ValidationError(msg)) if msg.contains("Invalid input")));

        let var_result = Executable::variables(&execute, input);
        assert!(matches!(var_result, Err(ValidationError(msg)) if msg.contains("Invalid input")));
    }

    #[test]
    fn execute_query_invalid_variables() {
        let execute = Execute::new(MutationMode::None, None);

        let input = json!({
            "query": "query GetUser($id: ID!) { user(id: $id) { id name } }",
            "variables": "garbage",
        });

        let result = Executable::variables(&execute, input);
        assert!(matches!(result, Err(ValidationError(msg)) if msg.contains("Invalid variables")));
    }

    #[test]
    fn execute_annotations_when_mutations_disabled() {
        for mode in [MutationMode::None, MutationMode::Explicit] {
            let annotations = Execute::new(mode, None)
                .tool
                .annotations
                .expect("execute tool must expose annotations");
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.idempotent_hint, Some(true));
            assert_eq!(annotations.open_world_hint, Some(true));
        }
    }

    #[test]
    fn execute_annotations_when_mutations_enabled() {
        let annotations = Execute::new(MutationMode::All, None)
            .tool
            .annotations
            .expect("execute tool must expose annotations");
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.destructive_hint, Some(true));
        assert_eq!(annotations.idempotent_hint, Some(false));
        assert_eq!(annotations.open_world_hint, Some(true));
    }
}
