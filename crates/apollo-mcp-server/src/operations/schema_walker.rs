//! JSON Schema generation utilities
//!
//! The types in this module generate JSON schemas for GraphQL types by walking
//! the types recursively.

use apollo_compiler::{
    Schema as GraphQLSchema,
    ast::{Type, Value as GraphQLValue},
};
use schemars::Schema;
use serde_json::{Map, Number, Value};

use crate::custom_scalar_map::CustomScalarMap;

mod name;
mod output;
mod r#type;

pub use output::selection_set_to_schema;

/// Convert a GraphQL type into a JSON Schema.
///
/// `default_value` is the GraphQL default of the variable or input field being
/// described, if it has one. It is emitted as the JSON Schema `default`.
///
/// Note: This is recursive, which might cause a stack overflow if the type is
/// sufficiently nested / complex.
pub fn type_to_schema(
    r#type: &Type,
    schema: &GraphQLSchema,
    definitions: &mut Map<String, Value>,
    custom_scalar_map: Option<&CustomScalarMap>,
    description: Option<String>,
    default_value: Option<&GraphQLValue>,
) -> Schema {
    with_default(
        r#type::Type {
            cache: definitions,
            custom_scalar_map,
            description: &description,
            schema,
            r#type,
        }
        .into(),
        default_value,
    )
}

/// Modifies a schema to include the default value from the GraphQL definition, if any
fn with_default(mut schema: Schema, default_value: Option<&GraphQLValue>) -> Schema {
    if let Some(default) = default_value.and_then(graphql_value_to_json) {
        schema
            .ensure_object()
            .insert("default".to_string(), default);
    }

    schema
}

/// Converts a constant GraphQL value into JSON.
///
/// Returns `None` for values with no JSON representation: variables, and numbers
/// outside the `f64` range.
fn graphql_value_to_json(value: &GraphQLValue) -> Option<Value> {
    match value {
        GraphQLValue::Null => Some(Value::Null),
        GraphQLValue::Boolean(boolean) => Some(Value::Bool(*boolean)),
        GraphQLValue::String(string) => Some(Value::String(string.clone())),
        GraphQLValue::Enum(name) => Some(Value::String(name.to_string())),
        GraphQLValue::Int(int) => int
            .as_str()
            .parse::<i64>()
            .ok()
            .map(Value::from)
            .or_else(|| {
                int.try_to_f64()
                    .ok()
                    .and_then(Number::from_f64)
                    .map(Value::Number)
            }),
        GraphQLValue::Float(float) => float
            .try_to_f64()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number),
        GraphQLValue::List(items) => items
            .iter()
            .map(|item| graphql_value_to_json(item))
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        GraphQLValue::Object(fields) => fields
            .iter()
            .map(|(name, value)| {
                graphql_value_to_json(value).map(|value| (name.to_string(), value))
            })
            .collect::<Option<Map<_, _>>>()
            .map(Value::Object),
        GraphQLValue::Variable(_) => None,
    }
}

/// Modifies a schema to include an optional description
fn with_desc(mut schema: Schema, description: &Option<String>) -> Schema {
    if let Some(desc) = description {
        schema
            .ensure_object()
            .entry("description")
            .or_insert(desc.clone().into());
    }

    schema
}

#[cfg(test)]
mod tests {
    use apollo_compiler::{ExecutableDocument, Node, ast::VariableDefinition, validation::Valid};
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const SDL: &str = r#"
        enum Status {
            OPEN
            CLOSED
        }

        input Filter {
            status: Status = OPEN
            limit: Int! = 20
            tags: [String!]
            name: String!
        }

        type Query {
            things(filter: Filter): String
        }
    "#;

    fn schema() -> Valid<GraphQLSchema> {
        GraphQLSchema::parse_and_validate(SDL, "schema.graphql").unwrap()
    }

    /// Parses `definition` as the single variable of an operation, e.g. `$v: Int = 10`.
    fn variable(schema: &Valid<GraphQLSchema>, definition: &str) -> Node<VariableDefinition> {
        let document = ExecutableDocument::parse(
            schema,
            format!("query Q({definition}) {{ things }}"),
            "operation.graphql",
        )
        .unwrap();
        document.operations.get(Some("Q")).unwrap().variables[0].clone()
    }

    /// Runs `type_to_schema` for a variable definition, returning the property schema
    /// and the collected definitions.
    fn convert(definition: &str, description: Option<&str>) -> (Value, Map<String, Value>) {
        let schema = schema();
        let variable = variable(&schema, definition);
        let mut definitions = Map::new();
        let converted = type_to_schema(
            &variable.ty,
            &schema,
            &mut definitions,
            None,
            description.map(str::to_string),
            variable.default_value.as_deref(),
        );
        (json!(converted), definitions)
    }

    #[test]
    fn nullable_named_type_allows_null() {
        let (schema, _) = convert("$v: String", None);

        assert_eq!(
            schema,
            json!({"oneOf": [{"type": "string"}, {"type": "null"}]})
        );
    }

    #[test]
    fn non_null_named_type_is_bare() {
        let (schema, _) = convert("$v: String!", None);

        assert_eq!(schema, json!({"type": "string"}));
    }

    #[test]
    fn nullable_list_of_nullable_items_allows_null_at_both_levels() {
        let (schema, _) = convert("$v: [Int]", None);

        assert_eq!(
            schema,
            json!({
                "oneOf": [
                    {
                        "type": "array",
                        "items": {"oneOf": [{"type": "integer"}, {"type": "null"}]},
                    },
                    {"type": "null"},
                ]
            })
        );
    }

    #[test]
    fn non_null_list_of_non_null_items_is_bare() {
        let (schema, _) = convert("$v: [Int!]!", None);

        assert_eq!(
            schema,
            json!({"type": "array", "items": {"type": "integer"}})
        );
    }

    #[test]
    fn description_is_attached_to_the_outermost_schema() {
        let (schema, _) = convert("$v: [Int]", Some("Some numbers"));

        assert_eq!(
            schema,
            json!({
                "description": "Some numbers",
                "oneOf": [
                    {
                        "type": "array",
                        "items": {"oneOf": [{"type": "integer"}, {"type": "null"}]},
                    },
                    {"type": "null"},
                ]
            })
        );
    }

    #[test]
    fn nullable_reference_wraps_the_ref() {
        let (schema, _) = convert("$v: Status", None);

        assert_eq!(
            schema,
            json!({"oneOf": [{"$ref": "#/definitions/Status"}, {"type": "null"}]})
        );
    }

    #[rstest]
    #[case::int("$v: Int = 10", json!(10))]
    #[case::float("$v: Float = 1.5", json!(1.5))]
    #[case::string("$v: String = \"x\"", json!("x"))]
    #[case::boolean("$v: Boolean = false", json!(false))]
    #[case::enum_value("$v: Status = OPEN", json!("OPEN"))]
    #[case::list("$v: [Int] = [1, 2]", json!([1, 2]))]
    #[case::object("$v: Filter = { name: \"n\", limit: 5 }", json!({"name": "n", "limit": 5}))]
    #[case::null("$v: String = null", json!(null))]
    fn default_value_is_emitted_as_json(#[case] definition: &str, #[case] expected: Value) {
        let (schema, _) = convert(definition, None);

        assert_eq!(schema["default"], expected);
    }

    #[test]
    fn no_default_value_means_no_default_keyword() {
        let (schema, _) = convert("$v: Int", None);

        assert!(schema.get("default").is_none(), "{schema}");
    }

    #[test]
    fn input_field_default_value_is_emitted() {
        let (_, definitions) = convert("$v: Filter!", None);

        assert_eq!(
            definitions["Filter"]["properties"]["status"]["default"],
            json!("OPEN")
        );
    }

    #[test]
    fn input_field_with_default_value_is_not_required() {
        let (_, definitions) = convert("$v: Filter!", None);

        assert_eq!(definitions["Filter"]["required"], json!(["name"]));
    }
}
