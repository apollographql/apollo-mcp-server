//! JSON Schema generation for GraphQL output types (selection sets)
//!
//! This module generates JSON schemas from GraphQL operation selection sets,
//! enabling MCP tools to declare their output schema.

use std::collections::HashMap;

use apollo_compiler::{
    Name as GraphQLName, Node, Schema as GraphQLSchema,
    ast::{Field, Selection, Type as GraphQLType},
    schema::ExtendedType,
};
use schemars::{Schema as JSONSchema, json_schema};
use serde_json::{Map, Value};
use tracing::warn;

use crate::custom_scalar_map::CustomScalarMap;

/// Generate a JSON Schema for the output of a GraphQL operation.
///
/// This walks the selection set and generates a schema that describes
/// the expected response structure.
pub fn selection_set_to_schema(
    selection_set: &[Selection],
    parent_type: &ExtendedType,
    graphql_schema: &GraphQLSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
) -> JSONSchema {
    let mut definitions = Map::new();

    let schema = build_selection_set_schema(
        selection_set,
        parent_type,
        graphql_schema,
        custom_scalar_map,
        named_fragments,
        &mut definitions,
    );

    // Wrap in standard GraphQL response envelope
    let mut response_schema = json_schema!({
        "type": "object",
        "properties": {
            "data": schema,
            "errors": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "locations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "line": { "type": "integer" },
                                    "column": { "type": "integer" }
                                }
                            }
                        },
                        "path": {
                            "type": "array",
                            "items": {
                                "oneOf": [
                                    { "type": "string" },
                                    { "type": "integer" }
                                ]
                            }
                        },
                        "extensions": { "type": "object" }
                    },
                    "required": ["message"]
                }
            },
            "extensions": { "type": "object" }
        }
    });

    // Add definitions if we collected any
    if !definitions.is_empty() {
        response_schema
            .ensure_object()
            .insert("definitions".to_string(), definitions.into());
    }

    response_schema
}

/// Build a schema for a selection set (object fields)
fn build_selection_set_schema(
    selection_set: &[Selection],
    parent_type: &ExtendedType,
    graphql_schema: &GraphQLSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
) -> JSONSchema {
    let mut properties = Map::new();
    let mut required = Vec::new();

    // Always include __typename if it could be useful
    let type_name = parent_type.name().to_string();

    for selection in selection_set {
        match selection {
            Selection::Field(field) => {
                let field_name = field.name.to_string();
                let response_key = field
                    .alias
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| field_name.clone());

                // Skip __typename - it's always a string
                if field_name == "__typename" {
                    properties.insert(
                        response_key,
                        json_schema!({"type": "string", "description": "The typename of this object"}).into(),
                    );
                    continue;
                }

                // Get field definition from parent type
                if let Some(field_def) = get_field_definition(parent_type, &field_name) {
                    let field_schema = build_field_schema(
                        field,
                        &field_def.ty,
                        graphql_schema,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                        field_def.description.as_ref().map(|n| n.to_string()),
                    );

                    properties.insert(response_key.clone(), field_schema.into());

                    // Non-null fields are required in the response
                    if field_def.ty.is_non_null() {
                        required.push(response_key);
                    }
                } else {
                    warn!(
                        field = field_name,
                        parent_type = type_name,
                        "Field not found in parent type"
                    );
                }
            }
            Selection::FragmentSpread(fragment_spread) => {
                // Merge fields from named fragment
                if let Some(fragment_def) =
                    named_fragments.get(fragment_spread.fragment_name.as_str())
                    && let Some(target_type) = graphql_schema
                        .types
                        .get(fragment_def.type_condition.as_str())
                {
                    let fragment_schema = build_selection_set_schema(
                        &fragment_def.selection_set,
                        target_type,
                        graphql_schema,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                    );

                    // Merge properties from fragment
                    if let Some(props) = fragment_schema
                        .as_object()
                        .and_then(|o| o.get("properties"))
                        .and_then(|v| v.as_object())
                    {
                        for (key, value) in props {
                            properties.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            Selection::InlineFragment(inline_fragment) => {
                // For inline fragments, we need to handle type conditions
                let target_type = if let Some(type_condition) = &inline_fragment.type_condition {
                    graphql_schema.types.get(type_condition.as_str())
                } else {
                    Some(parent_type)
                };

                if let Some(target_type) = target_type {
                    let fragment_schema = build_selection_set_schema(
                        &inline_fragment.selection_set,
                        target_type,
                        graphql_schema,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                    );

                    // Merge properties from inline fragment
                    if let Some(props) = fragment_schema
                        .as_object()
                        .and_then(|o| o.get("properties"))
                        .and_then(|v| v.as_object())
                    {
                        for (key, value) in props {
                            properties.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        }
    }

    let mut schema = json_schema!({"type": "object"});
    let obj = schema.ensure_object();

    if !properties.is_empty() {
        obj.insert("properties".to_string(), properties.into());
    }

    if !required.is_empty() {
        obj.insert(
            "required".to_string(),
            required
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>()
                .into(),
        );
    }

    schema
}

/// Build schema for a specific field based on its type
fn build_field_schema(
    field: &Node<Field>,
    field_type: &GraphQLType,
    graphql_schema: &GraphQLSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
    description: Option<String>,
) -> JSONSchema {
    let schema = type_to_output_schema(
        field_type,
        &field.selection_set,
        graphql_schema,
        custom_scalar_map,
        named_fragments,
        definitions,
    );

    with_description(schema, description)
}

/// Convert a GraphQL type to a JSON Schema for output
fn type_to_output_schema(
    graphql_type: &GraphQLType,
    selection_set: &[Selection],
    graphql_schema: &GraphQLSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
) -> JSONSchema {
    match graphql_type {
        // Non-null types - just unwrap
        GraphQLType::NonNullNamed(name) => named_type_to_output_schema(
            name,
            selection_set,
            graphql_schema,
            custom_scalar_map,
            named_fragments,
            definitions,
        ),
        GraphQLType::NonNullList(inner) => {
            let items = type_to_output_schema(
                inner.as_ref(),
                selection_set,
                graphql_schema,
                custom_scalar_map,
                named_fragments,
                definitions,
            );
            json_schema!({
                "type": "array",
                "items": items
            })
        }

        // Nullable types - allow null
        GraphQLType::Named(name) => {
            let inner = named_type_to_output_schema(
                name,
                selection_set,
                graphql_schema,
                custom_scalar_map,
                named_fragments,
                definitions,
            );
            json_schema!({
                "oneOf": [inner, {"type": "null"}]
            })
        }
        GraphQLType::List(inner) => {
            let items = type_to_output_schema(
                inner.as_ref(),
                selection_set,
                graphql_schema,
                custom_scalar_map,
                named_fragments,
                definitions,
            );
            json_schema!({
                "oneOf": [
                    {"type": "array", "items": items},
                    {"type": "null"}
                ]
            })
        }
    }
}

/// Convert a named GraphQL type to JSON Schema
fn named_type_to_output_schema(
    name: &GraphQLName,
    selection_set: &[Selection],
    graphql_schema: &GraphQLSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
) -> JSONSchema {
    match name.as_str() {
        // Built-in scalars
        "String" => json_schema!({"type": "string"}),
        "Int" => json_schema!({"type": "integer"}),
        "Float" => json_schema!({"type": "number"}),
        "Boolean" => json_schema!({"type": "boolean"}),
        // ID can be serialized as string or integer depending on the GraphQL server
        "ID" => json_schema!({"oneOf": [{"type": "string"}, {"type": "integer"}]}),

        // Check cache first
        other if definitions.contains_key(other) => {
            JSONSchema::new_ref(format!("#/definitions/{other}"))
        }

        // Look up in schema
        other => match graphql_schema.types.get(other) {
            // Object types - recurse into selection set
            Some(ExtendedType::Object(obj)) => {
                if selection_set.is_empty() {
                    // No selection set - just reference the type
                    warn!(
                        type_name = other,
                        "Object type without selection set in output schema"
                    );
                    json_schema!({})
                } else {
                    build_selection_set_schema(
                        selection_set,
                        &ExtendedType::Object(obj.clone()),
                        graphql_schema,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                    )
                }
            }

            // Interface types - similar to objects
            Some(ExtendedType::Interface(iface)) => {
                if selection_set.is_empty() {
                    json_schema!({})
                } else {
                    build_selection_set_schema(
                        selection_set,
                        &ExtendedType::Interface(iface.clone()),
                        graphql_schema,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                    )
                }
            }

            // Union types - oneOf the possible types based on inline fragments
            Some(ExtendedType::Union(_union_def)) => {
                if selection_set.is_empty() {
                    json_schema!({})
                } else {
                    // Collect schemas for each possible type from inline fragments
                    let mut type_schemas = Vec::new();

                    for selection in selection_set {
                        if let Selection::InlineFragment(fragment) = selection
                            && let Some(type_condition) = &fragment.type_condition
                            && let Some(member_type) =
                                graphql_schema.types.get(type_condition.as_str())
                        {
                            let member_schema = build_selection_set_schema(
                                &fragment.selection_set,
                                member_type,
                                graphql_schema,
                                custom_scalar_map,
                                named_fragments,
                                definitions,
                            );
                            type_schemas.push(member_schema);
                        }
                    }

                    if type_schemas.is_empty() {
                        // No inline fragments - just return empty schema
                        json_schema!({})
                    } else if type_schemas.len() == 1 {
                        type_schemas.remove(0)
                    } else {
                        json_schema!({"oneOf": type_schemas})
                    }
                }
            }

            // Enum types
            // Note: We only include the enum's type description (not per-value descriptions)
            // to avoid token bloat with large enums. The `enum` constraint already lists
            // all valid values, which is sufficient for understanding output.
            Some(ExtendedType::Enum(enum_def)) => {
                let values: Vec<Value> = enum_def
                    .values
                    .iter()
                    .map(|(_, v)| serde_json::json!(v.value))
                    .collect();

                let mut enum_schema = json_schema!({
                    "type": "string",
                    "enum": values
                });

                // Only include the enum's type description, not per-value descriptions
                if let Some(desc) = &enum_def.description {
                    enum_schema
                        .ensure_object()
                        .insert("description".to_string(), desc.to_string().into());
                }

                definitions.insert(other.to_string(), enum_schema.into());

                JSONSchema::new_ref(format!("#/definitions/{other}"))
            }

            // Custom scalars
            Some(ExtendedType::Scalar(scalar)) => {
                let description = scalar.description.as_ref().map(|n| n.to_string());

                if let Some(custom_map) = custom_scalar_map
                    && let Some(custom_schema) = custom_map.get(other)
                {
                    return with_description(custom_schema.clone(), description);
                }

                // Unknown scalar - return empty schema with description
                with_description(json_schema!({}), description)
            }

            // InputObject shouldn't appear in output, but handle gracefully
            Some(ExtendedType::InputObject(_)) => {
                warn!(
                    type_name = other,
                    "InputObject type found in output schema - this is unexpected"
                );
                json_schema!({})
            }

            None => {
                warn!(type_name = other, "Type not found in schema");
                json_schema!({})
            }
        },
    }
}

/// Get field definition from a parent type (Object or Interface)
fn get_field_definition(
    parent_type: &ExtendedType,
    field_name: &str,
) -> Option<Node<apollo_compiler::schema::FieldDefinition>> {
    match parent_type {
        ExtendedType::Object(obj) => obj.fields.get(field_name).map(|f| f.node.clone()),
        ExtendedType::Interface(iface) => iface.fields.get(field_name).map(|f| f.node.clone()),
        _ => None,
    }
}

/// Add description to a schema if provided
fn with_description(mut schema: JSONSchema, description: Option<String>) -> JSONSchema {
    if let Some(desc) = description {
        schema
            .ensure_object()
            .entry("description")
            .or_insert(desc.into());
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_compiler::parser::Parser;

    fn parse_schema(sdl: &str) -> GraphQLSchema {
        GraphQLSchema::parse_and_validate(sdl, "schema.graphql")
            .unwrap()
            .into_inner()
    }

    fn parse_operation(query: &str) -> (apollo_compiler::ast::Document, Vec<Selection>) {
        let doc = Parser::new().parse_ast(query, "query.graphql").unwrap();
        let selection_set = doc
            .definitions
            .iter()
            .find_map(|def| match def {
                apollo_compiler::ast::Definition::OperationDefinition(op) => {
                    Some(op.selection_set.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        (doc, selection_set)
    }

    #[test]
    fn simple_query_output_schema() {
        let schema = parse_schema(
            r#"
            type Query {
                "Get a user by ID"
                user(id: ID!): User
            }

            "A user in the system"
            type User {
                "The user's unique identifier"
                id: ID!
                "The user's display name"
                name: String!
                "The user's email address"
                email: String
            }
            "#,
        );

        let (_, selection_set) = parse_operation(
            r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    name
                    email
                }
            }
            "#,
        );

        let query_type = schema.types.get("Query").unwrap();
        let output_schema =
            selection_set_to_schema(&selection_set, query_type, &schema, None, &HashMap::new());

        insta::assert_snapshot!(serde_json::to_string_pretty(&output_schema).unwrap());
    }

    #[test]
    fn nested_object_output_schema() {
        let schema = parse_schema(
            r#"
            type Query {
                user(id: ID!): User
            }

            type User {
                id: ID!
                profile: Profile!
            }

            type Profile {
                bio: String
                avatar: String!
            }
            "#,
        );

        let (_, selection_set) = parse_operation(
            r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    profile {
                        bio
                        avatar
                    }
                }
            }
            "#,
        );

        let query_type = schema.types.get("Query").unwrap();
        let output_schema =
            selection_set_to_schema(&selection_set, query_type, &schema, None, &HashMap::new());

        insta::assert_snapshot!(serde_json::to_string_pretty(&output_schema).unwrap());
    }
}
