use std::collections::HashMap;

use apollo_compiler::ast::{Document, OperationType, Selection};
use apollo_compiler::validation::WithErrors;
use apollo_compiler::{
    Name, Node, Schema as GraphqlSchema,
    ast::{Definition, OperationDefinition, Type},
    parser::Parser,
};
use rmcp::{
    model::Tool,
    schemars::schema::{
        ArrayValidation, InstanceType, Metadata, ObjectValidation, RootSchema, Schema,
        SchemaObject, SingleOrVec, SubschemaValidation,
    },
    serde_json::{self, Value},
};
use serde_derive::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("Could not parse GraphQL document: {0}")]
    GraphQLDocument(WithErrors<Document>),

    #[error("Could not parse GraphQL schema: {0}")]
    GraphQLSchema(WithErrors<Schema>),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Operation is missing its required name: {0}")]
    MissingName(String),

    #[error("No operations defined")]
    NoOperations,

    #[error("Invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Too many operations. Expected 1 but got {0}")]
    TooManyOperations(usize),
}

#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    tool: Tool,
    source_text: String,
}
impl AsRef<Tool> for Operation {
    fn as_ref(&self) -> &Tool {
        &self.tool
    }
}

impl From<Operation> for Tool {
    fn from(value: Operation) -> Tool {
        value.tool
    }
}

impl Operation {
    pub fn from_spec(
        source_text: &str,
        graphql_schema: &GraphqlSchema,
        custom_scalar_map: Option<&HashMap<String, SchemaObject>>,
    ) -> Result<Self, OperationError> {
        let document = Parser::new()
            .parse_ast(source_text, "operation.graphql")
            .map_err(OperationError::GraphQLDocument)?;

        let mut operation_defs = document.definitions.into_iter().filter_map(|def| match def {
            Definition::OperationDefinition(operation_def) => Some(operation_def),
            Definition::FragmentDefinition(_) => None,
            _ => {
                tracing::error!(
                    spec=?def,
                    "Schema definitions were passed in, only operations and fragments are allowed"
                );
                None
            }
        });

        let operation = match (operation_defs.next(), operation_defs.next()) {
            (None, _) => return Err(OperationError::NoOperations),
            (_, Some(_)) => {
                return Err(OperationError::TooManyOperations(
                    1 + operation_defs.count(),
                ));
            }
            (Some(op), None) => op,
        };

        let operation_name = operation
            .name
            .as_ref()
            .ok_or_else(|| {
                OperationError::MissingName(operation.serialize().no_indent().to_string())
            })?
            .to_string();

        let description = Self::tool_description(graphql_schema, &operation).unwrap_or_default();

        let object = serde_json::to_value(get_json_schema(
            &operation,
            graphql_schema,
            custom_scalar_map,
        ))?;
        let serde_json::Value::Object(schema) = object else {
            return Err(OperationError::Internal(
                "Schemars should have returned an object".to_string(),
            ));
        };

        Ok(Operation {
            tool: Tool::new(operation_name, description, schema),
            source_text: source_text.to_string(),
        })
    }

    /// Generate a description for an operation based on documentation in the schema
    fn tool_description(
        graphql_schema: &GraphqlSchema,
        operation_def: &Node<OperationDefinition>,
    ) -> Option<String> {
        let description = operation_def
            .selection_set
            .iter()
            .filter_map(|selection| {
                match selection {
                    Selection::Field(field) => {
                        let field_name = field.name.to_string();
                        let operation_type = operation_def.operation_type;
                        let component = match operation_type {
                            OperationType::Query => graphql_schema.schema_definition.query.clone(),
                            OperationType::Mutation => {
                                graphql_schema.schema_definition.mutation.clone()
                            }
                            OperationType::Subscription => {
                                graphql_schema.schema_definition.subscription.clone()
                            }
                        }?;
                        let query = graphql_schema.get_object(&component)?;
                        query
                            .fields
                            .iter()
                            .find(|(name, _)| {
                                let name = name.to_string();
                                name == field_name
                            })
                            .map(|(_, field_definition)| field_definition.node.clone())
                            .and_then(|field| field.description.clone())
                            .map(|n| n.to_string())
                    }
                    _ => None, // TODO: handle fragments
                }
            })
            .collect::<Vec<String>>()
            .join("\n");
        if description.is_empty() {
            None
        } else {
            Some(description)
        }
    }

    pub async fn execute(
        &self,
        endpoint: &str,
        variables: serde_json::Value,
    ) -> Result<String, reqwest::Error> {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "query": self.source_text,
            "variables": variables,
        })
        .to_string();

        match client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(response) => response.text().await,
            Err(e) => Err(e),
        }
    }
}

fn get_json_schema(
    operation: &Node<OperationDefinition>,
    graphql_schema: &GraphqlSchema,
    custom_scalar_map: Option<&HashMap<String, SchemaObject>>,
) -> RootSchema {
    let mut obj = ObjectValidation::default();

    operation.variables.iter().for_each(|variable| {
        let variable_name = variable.name.to_string();
        let type_name = variable.ty.inner_named_type();
        let schema = type_to_schema(
            // For the root description, for now we can use the type description.
            description(type_name, graphql_schema),
            variable.ty.as_ref(),
            graphql_schema,
            custom_scalar_map,
        );
        obj.properties.insert(variable_name.clone(), schema);
        if variable.ty.is_non_null() {
            obj.required.insert(variable_name);
        }
    });

    RootSchema {
        schema: SchemaObject {
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
            object: Some(Box::new(obj)),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn schema_factory(
    description: Option<String>,
    instance_type: InstanceType,
    object_validation: Option<ObjectValidation>,
    array_validation: Option<ArrayValidation>,
    subschema_validation: Option<SubschemaValidation>,
    enum_values: Option<Vec<Value>>,
) -> Schema {
    Schema::Object(SchemaObject {
        instance_type: Some(SingleOrVec::Single(Box::new(instance_type))),
        object: object_validation.map(Box::new),
        array: array_validation.map(Box::new),
        subschemas: subschema_validation.map(Box::new),
        enum_values,
        metadata: Some(Box::new(Metadata {
            description,
            ..Default::default()
        })),
        ..Default::default()
    })
}
fn description(name: &Name, graphql_schema: &GraphqlSchema) -> Option<String> {
    let description = if let Some(input_object) = graphql_schema.get_input_object(name) {
        input_object.description.clone()
    } else if let Some(scalar) = graphql_schema.get_scalar(name) {
        scalar.description.clone()
    } else {
        None
    };
    description.map(|n| n.to_string())
}
fn type_to_schema(
    description: Option<String>,
    variable_type: &Type,
    graphql_schema: &GraphqlSchema,
    custom_scalar_map: Option<&HashMap<String, SchemaObject>>,
) -> Schema {
    match variable_type {
        Type::NonNullNamed(named) | Type::Named(named) => match named.as_str() {
            "String" | "ID" => {
                schema_factory(description, InstanceType::String, None, None, None, None)
            }
            "Int" | "Float" => {
                schema_factory(description, InstanceType::Number, None, None, None, None)
            }
            "Boolean" => schema_factory(description, InstanceType::Boolean, None, None, None, None),
            _ => {
                if let Some(input_type) = graphql_schema.get_input_object(named) {
                    let mut obj = ObjectValidation::default();

                    input_type.fields.iter().for_each(|(name, field)| {
                        let description = field.description.as_ref().map(|n| n.to_string());
                        obj.properties.insert(
                            name.to_string(),
                            type_to_schema(
                                description,
                                field.ty.as_ref(),
                                graphql_schema,
                                custom_scalar_map,
                            ),
                        );

                        if field.is_required() {
                            obj.required.insert(name.to_string());
                        }
                    });

                    schema_factory(
                        description,
                        InstanceType::Object,
                        Some(obj),
                        None,
                        None,
                        None,
                    )
                } else if graphql_schema.get_scalar(named).is_some() {
                    if let Some(custom_scalar_map) = custom_scalar_map {
                        if let Some(custom_scalar_schema_object) =
                            custom_scalar_map.get(named.as_str())
                        {
                            let mut custom_schema = custom_scalar_schema_object.clone();
                            let mut meta = *custom_schema.metadata.unwrap_or_default();
                            // If description isn't included in custom schema, inject the one from the schema
                            if meta.description.is_none() {
                                meta.description = description;
                            }
                            custom_schema.metadata = Some(Box::new(meta));
                            Schema::Object(custom_schema)
                        } else {
                            panic!("custom scalar missing from custom_scalar_map")
                        }
                    } else {
                        panic!(
                            "custom scalars aren't currently supported without a custom_scalar_map"
                        )
                    }
                } else if let Some(enum_type) = graphql_schema.get_enum(named) {
                    schema_factory(
                        description,
                        InstanceType::String,
                        None,
                        None,
                        None,
                        Some(
                            enum_type
                                .values
                                .iter()
                                .map(|(_name, value)| serde_json::json!(value.value))
                                .collect(),
                        ),
                    )
                } else {
                    // TODO: Should this be an "any" type or an error?
                    panic!("Type not found in schema! {named}")
                }
            }
        },
        Type::NonNullList(list_type) | Type::List(list_type) => {
            let inner_type_schema =
                type_to_schema(description, list_type, graphql_schema, custom_scalar_map);
            schema_factory(
                None,
                InstanceType::Array,
                None,
                list_type.is_non_null().then(|| ArrayValidation {
                    items: Some(SingleOrVec::Single(Box::new(inner_type_schema.clone()))),
                    ..Default::default()
                }),
                (!list_type.is_non_null()).then(|| SubschemaValidation {
                    one_of: Some(vec![
                        inner_type_schema,
                        Schema::Object(SchemaObject {
                            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Null))),
                            ..Default::default()
                        }),
                    ]),
                    ..Default::default()
                }),
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use apollo_compiler::parser::Parser;
    use rmcp::{
        model::Tool,
        schemars::schema::{InstanceType, SchemaObject, SingleOrVec},
        serde_json,
    };

    use crate::operations::Operation;

    fn expect_json_schema(
        source_text: &str,
        expected_json: serde_json::Value,
        expected_name: &str,
        expected_description: &str,
        custom_scalar_map: Option<&HashMap<String, SchemaObject>>,
    ) {
        let mut parser = Parser::new();
        let document = parser
            .parse_ast(
                "
                    type Query { id: String }
                    \"\"\"
                    RealCustomScalar exists
                    \"\"\"
                    scalar RealCustomScalar
                    input RealInputObject {
                        \"\"\"
                        optional is a input field that is optional
                        \"\"\"
                        optional: String
                        \"\"\"
                        required is a input field that is required
                        \"\"\"
                        required: String!
                    }

                    enum RealEnum {
                        \"\"\"
                        ENUM_VALUE_1 is a value
                        \"\"\"
                        ENUM_VALUE_1
                        \"\"\"
                        ENUM_VALUE_2 is a value
                        \"\"\"
                        ENUM_VALUE_2
                    }
                ",
                "operation.graphql",
            )
            .expect("failed to parse operation");
        let graphql_schema = document.to_schema().unwrap();

        let operation =
            Operation::from_spec(source_text, &graphql_schema, custom_scalar_map).unwrap();
        let Tool {
            name,
            description,
            input_schema,
        } = operation.into();
        assert_eq!(serde_json::json!(input_schema), expected_json);
        assert_eq!(name, expected_name);
        assert_eq!(description, expected_description)
    }

    #[test]
    fn no_variables() {
        expect_json_schema(
            "query QueryName { id }",
            serde_json::json!({"type": "object"}),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn nullable_named_type() {
        expect_json_schema(
            "query QueryName($id: ID) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The `ID` scalar type represents a unique identifier, often used to refetch an object or as key for a cache. The ID type appears in a JSON response as a String; however, it is not intended to be human-readable. When expected as an input type, any string (such as `\"4\"`) or integer (such as `4`) input value will be accepted as an ID."
                    }
                }
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn non_nullable_named_type() {
        expect_json_schema(
            "query QueryName($id: ID!) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The `ID` scalar type represents a unique identifier, often used to refetch an object or as key for a cache. The ID type appears in a JSON response as a String; however, it is not intended to be human-readable. When expected as an input type, any string (such as `\"4\"`) or integer (such as `4`) input value will be accepted as an ID."
                    }
                },
                "required": ["id"]
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn non_nullable_list_of_nullable_named_type() {
        expect_json_schema(
            "query QueryName($id: [ID]!) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "array",
                        "oneOf": [
                            {
                                "type": "string",
                                "description": "The `ID` scalar type represents a unique identifier, often used to refetch an object or as key for a cache. The ID type appears in a JSON response as a String; however, it is not intended to be human-readable. When expected as an input type, any string (such as `\"4\"`) or integer (such as `4`) input value will be accepted as an ID.",
                            },
                            {"type": "null"}
                        ]
                    }
                },
                "required": ["id"]
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn non_nullable_list_of_non_nullable_named_type() {
        expect_json_schema(
            "query QueryName($id: [ID!]!) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "description": "The `ID` scalar type represents a unique identifier, often used to refetch an object or as key for a cache. The ID type appears in a JSON response as a String; however, it is not intended to be human-readable. When expected as an input type, any string (such as `\"4\"`) or integer (such as `4`) input value will be accepted as an ID.",
                        }
                    }
                },
                "required": ["id"]
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn nullable_list_of_nullable_named_type() {
        expect_json_schema(
            "query QueryName($id: [ID]) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "array",
                        "oneOf": [
                            {
                                "type": "string",
                                "description": "The `ID` scalar type represents a unique identifier, often used to refetch an object or as key for a cache. The ID type appears in a JSON response as a String; however, it is not intended to be human-readable. When expected as an input type, any string (such as `\"4\"`) or integer (such as `4`) input value will be accepted as an ID.",
                            },
                            {"type": "null"}
                        ]
                    }
                },
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn nullable_list_of_non_nullable_named_type() {
        expect_json_schema(
            "query QueryName($id: [ID!]) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "description": "The `ID` scalar type represents a unique identifier, often used to refetch an object or as key for a cache. The ID type appears in a JSON response as a String; however, it is not intended to be human-readable. When expected as an input type, any string (such as `\"4\"`) or integer (such as `4`) input value will be accepted as an ID.",
                        }
                    }
                },
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn nullable_list_of_nullable_lists_of_nullable_named_types() {
        expect_json_schema(
            "query QueryName($id: [[ID]]) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "array",
                        "oneOf": [
                            {"type": "array", "oneOf": [{
                                "type": "string",
                                "description": "The `ID` scalar type represents a unique identifier, often used to refetch an object or as key for a cache. The ID type appears in a JSON response as a String; however, it is not intended to be human-readable. When expected as an input type, any string (such as `\"4\"`) or integer (such as `4`) input value will be accepted as an ID.",
                            }, {"type": "null"}]},
                            {"type": "null"}
                        ]
                    }
                },
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn nullable_input_object() {
        expect_json_schema(
            "query QueryName($id: RealInputObject) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "object",
                        "properties": {
                            "optional": {
                                "type": "string",
                                "description": "optional is a input field that is optional"
                             },
                            "required": { "type": "string", "description": "required is a input field that is required" }
                        },
                        "required": ["required"]
                    }
                },
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    fn non_nullable_enum() {
        expect_json_schema(
            "query QueryName($id: RealEnum!) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "enum": [
                            "ENUM_VALUE_1",
                            "ENUM_VALUE_2",
                        ]

                    },
                },
                "required": ["id"]
            }),
            "QueryName",
            "",
            None,
        )
    }

    #[test]
    #[should_panic(expected = "too many operations in document: 2")]
    fn multiple_operations_should_panic() {
        expect_json_schema(
            "query QueryName { id } query QueryName { id }",
            serde_json::json!({}),
            "",
            "",
            None,
        )
    }

    #[test]
    #[should_panic(expected = "Operations require names")]
    fn unnamed_operations_should_panic() {
        expect_json_schema("query { id }", serde_json::json!({}), "", "", None)
    }

    #[test]
    #[should_panic(expected = "no operations in document")]
    fn no_operations_should_panic() {
        expect_json_schema(
            "fragment Test on Query { id }",
            serde_json::json!({}),
            "",
            "",
            None,
        )
    }

    #[test]
    #[should_panic(expected = "no operations in document")]
    fn schema_should_panic() {
        expect_json_schema(
            "type Query { id: String }",
            serde_json::json!({}),
            "",
            "",
            None,
        )
    }

    #[test]
    #[should_panic(expected = "Type not found in schema! FakeType")]
    fn unknown_type_should_panic() {
        expect_json_schema(
            "query QueryName($id: FakeType) { id }",
            serde_json::json!({}),
            "",
            "",
            None,
        )
    }

    #[test]
    #[should_panic(expected = "custom scalars aren't currently supported")]
    fn custom_scalar_without_map_should_panic() {
        expect_json_schema(
            "query QueryName($id: RealCustomScalar) { id }",
            serde_json::json!({}),
            "",
            "",
            None,
        )
    }

    #[test]
    #[should_panic(expected = "custom scalar missing from custom_scalar_map")]
    fn custom_scalar_with_map_but_not_found_should_panic() {
        expect_json_schema(
            "query QueryName($id: RealCustomScalar) { id }",
            serde_json::json!({}),
            "",
            "",
            Some(&HashMap::new()),
        )
    }

    #[test]
    fn custom_scalar_with_map() {
        let mut custom_scalar_map = HashMap::new();
        custom_scalar_map.insert(
            "RealCustomScalar".to_string(),
            SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
                ..Default::default()
            },
        );
        expect_json_schema(
            "query QueryName($id: RealCustomScalar) { id }",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "RealCustomScalar exists"
                    }
                },
            }),
            "QueryName",
            "",
            Some(&custom_scalar_map),
        )
    }
}
