use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::{McpError, OperationError};
use crate::graphql;
use crate::tree_shake::TreeShaker;
use apollo_compiler::ast::{FragmentDefinition, Selection};
use apollo_compiler::validation::Valid;
use apollo_compiler::{
    Name, Node, Schema as GraphqlSchema,
    ast::{Definition, OperationDefinition, Type},
    parser::Parser,
};
use mcp_apollo_registry::uplink::persisted_queries::{
    ManifestSource, PersistedQueryManifestPoller,
};
use regex::Regex;
use rmcp::{
    model::Tool,
    schemars::schema::{
        ArrayValidation, InstanceType, Metadata, ObjectValidation, RootSchema, Schema,
        SchemaObject, SingleOrVec, SubschemaValidation,
    },
    serde_json::{self, Value},
};
use serde::Serialize;
use std::path::PathBuf;
use tracing::{info, warn};

/// The source of the operations exposed as MCP tools
pub enum OperationSource {
    /// Static GraphQL document files (no hot reloading)
    Files(Vec<PathBuf>),

    /// Persisted Query manifest (including sources that support hot reloading)
    Manifest(ManifestSource),

    /// No operations provided
    None,
}

#[derive(Clone)]
pub enum OperationPoller {
    /// Static GraphQL document files (no hot reloading)
    Files(Vec<PathBuf>),

    /// Persisted Query manifest (including sources that support hot reloading)
    Manifest(PersistedQueryManifestPoller),

    /// No operations defined
    None,
}

impl OperationPoller {
    pub async fn operations(
        &self,
        schema: &Valid<apollo_compiler::Schema>,
        custom_scalars: Option<&CustomScalarMap>,
    ) -> Result<Vec<Operation>, OperationError> {
        match self {
            OperationPoller::Files(paths) => paths
                .iter()
                .map(|operation| {
                    let operation = std::fs::read_to_string(operation)?;
                    Operation::from_document(&operation, schema, None, custom_scalars)
                })
                .collect::<Result<Vec<Operation>, OperationError>>(),
            OperationPoller::Manifest(manifest_poller) => manifest_poller
                .get_all_operations()
                .into_iter()
                .map(|(pq_id, operation)| {
                    Operation::from_document(&operation, schema, Some(pq_id), custom_scalars)
                })
                .collect::<Result<Vec<Operation>, OperationError>>(),
            OperationPoller::None => Ok(Vec::default()),
        }
        .inspect(|operations| {
            if operations.is_empty() {
                warn!("No operations found - only introspection tools will be available");
            } else {
                info!(
                    "Loaded {} operations:\n{}",
                    operations.len(),
                    serde_json::to_string_pretty(&operations)
                        .unwrap_or(String::from("<unable to serialize>"))
                );
            }
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    tool: Tool,
    source_text: String,
    persisted_query_id: Option<String>,
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
    pub fn from_document(
        source_text: &str,
        graphql_schema: &GraphqlSchema,
        persisted_query_id: Option<String>,
        custom_scalar_map: Option<&CustomScalarMap>,
    ) -> Result<Self, OperationError> {
        let document = Parser::new()
            .parse_ast(source_text, "operation.graphql")
            .map_err(|e| OperationError::GraphQLDocument(Box::new(e)))?;

        let mut last_offset: Option<usize> = Some(0);
        let mut operation_defs = document.definitions.iter().filter_map(|def| {
            let description = match def.location() {
                Some(source_span) => {
                    let description = last_offset
                        .map(|start_offset| &source_text[start_offset..source_span.offset()]);
                    last_offset = Some(source_span.end_offset());
                    description
                }
                None => {
                    last_offset = None;
                    None
                }
            };

            match def {
                Definition::OperationDefinition(operation_def) => {
                    Some((operation_def, description))
                }
                Definition::FragmentDefinition(_) => None,
                _ => {
                    eprintln!("Schema definitions were passed in, but only operations and fragments are allowed");
                    None
                }
            }
        });

        let fragment_defs: Vec<&Node<FragmentDefinition>> = document
            .definitions
            .iter()
            .filter_map(|def| match def {
                Definition::FragmentDefinition(fragment_def) => Some(fragment_def),
                _ => None,
            })
            .collect();

        let (operation, comments) = match (operation_defs.next(), operation_defs.next()) {
            (None, _) => return Err(OperationError::NoOperations),
            (_, Some(_)) => {
                return Err(OperationError::TooManyOperations(
                    2 + operation_defs.count(),
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

        let description =
            Self::tool_description(comments, graphql_schema, operation, &fragment_defs);

        let object = serde_json::to_value(get_json_schema(
            operation,
            graphql_schema,
            custom_scalar_map,
        ))?;
        let Value::Object(schema) = object else {
            return Err(OperationError::Internal(
                "Schemars should have returned an object".to_string(),
            ));
        };

        Ok(Operation {
            tool: Tool::new(operation_name, description, schema),
            source_text: source_text.to_string(),
            persisted_query_id,
        })
    }

    /// Generate a description for an operation based on documentation in the schema
    fn tool_description(
        comments: Option<&str>,
        graphql_schema: &GraphqlSchema,
        operation_def: &Node<OperationDefinition>,
        fragment_defs: &[&Node<FragmentDefinition>],
    ) -> String {
        let comment_description = comments.and_then(|comments| {
            let content = Regex::new(r"(\n|^)\s*#").ok()?.replace_all(comments, "$1");
            let trimmed = content.trim();

            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        match comment_description {
            Some(description) => description,
            None => {
                let mut tree_shaker = TreeShaker::new(graphql_schema, fragment_defs);
                let descriptions = operation_def
                    .selection_set
                    .iter()
                    .filter_map(|selection| {
                        match selection {
                            Selection::Field(field) => {
                                let field_name = field.name.to_string();
                                let operation_type = operation_def.operation_type;
                                if let Some(root_name) =
                                    graphql_schema.root_operation(operation_type)
                                {
                                    // Find the root field referenced by the operation
                                    let root = graphql_schema.get_object(root_name)?;
                                    let field_definition = root
                                        .fields
                                        .iter()
                                        .find(|(name, _)| {
                                            let name = name.to_string();
                                            name == field_name
                                        })
                                        .map(|(_, field_definition)| field_definition.node.clone());

                                    // Add the root field description to the tool description
                                    let field_description = field_definition
                                        .clone()
                                        .and_then(|field| field.description.clone())
                                        .map(|node| node.to_string());

                                    // Add information about the return type
                                    let ty = field_definition.map(|field| field.ty.clone());
                                    let type_description = ty.as_ref().map(Self::type_description);

                                    // Retain the return type in the tree shaker
                                    if let Some(ty) = ty {
                                        let type_name = ty.inner_named_type();
                                        if let Some(extended_type) =
                                            graphql_schema.types.get(type_name.as_str())
                                        {
                                            tree_shaker.retain(
                                                type_name.clone(),
                                                extended_type,
                                                &field.selection_set,
                                            )
                                        }
                                    }

                                    Some(
                                        vec![field_description, type_description]
                                            .into_iter()
                                            .flatten()
                                            .collect::<Vec<String>>()
                                            .join("\n"),
                                    )
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        }
                    })
                    .collect::<Vec<String>>()
                    .join("\n---\n");

                // Add the tree-shaken types to the end of the tool description
                let mut lines = vec![];
                lines.push(descriptions);

                let mut shaken = tree_shaker.shaken().peekable();
                if shaken.peek().is_some() {
                    lines.push(String::from("---"));
                }
                for ty in shaken {
                    lines.push(ty.serialize().to_string());
                }

                lines.join("\n")
            }
        }
    }

    fn type_description(ty: &Type) -> String {
        let type_name = ty.inner_named_type();
        let mut lines = vec![];
        let optional = if ty.is_non_null() {
            ""
        } else {
            "is optional and "
        };
        let array = if ty.is_list() {
            "is an array of type"
        } else {
            "has type"
        };
        lines.push(format!(
            "The returned value {}{} `{}`",
            optional, array, type_name
        ));

        lines.join("\n")
    }
}

fn get_json_schema(
    operation: &Node<OperationDefinition>,
    graphql_schema: &GraphqlSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
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
    instance_type: Option<InstanceType>,
    object_validation: Option<ObjectValidation>,
    array_validation: Option<ArrayValidation>,
    subschema_validation: Option<SubschemaValidation>,
    enum_values: Option<Vec<Value>>,
) -> Schema {
    Schema::Object(SchemaObject {
        instance_type: instance_type
            .map(|instance_type| SingleOrVec::Single(Box::new(instance_type))),
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
    if let Some(input_object) = graphql_schema.get_input_object(name) {
        input_object.description.as_ref().map(|d| d.to_string())
    } else if let Some(scalar) = graphql_schema.get_scalar(name) {
        scalar.description.as_ref().map(|d| d.to_string())
    } else if let Some(enum_type) = graphql_schema.get_enum(name) {
        let values = enum_type
            .values
            .iter()
            .map(|(name, value)| {
                format!(
                    "{}: {}",
                    name,
                    value
                        .description
                        .as_ref()
                        .map(|d| d.to_string())
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "{}\n\nValues:\n{}",
            enum_type
                .description
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_default(),
            values
        ))
    } else {
        None
    }
}
fn type_to_schema(
    description: Option<String>,
    variable_type: &Type,
    graphql_schema: &GraphqlSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
) -> Schema {
    match variable_type {
        Type::NonNullNamed(named) | Type::Named(named) => match named.as_str() {
            "String" | "ID" => schema_factory(
                description,
                Some(InstanceType::String),
                None,
                None,
                None,
                None,
            ),
            "Int" | "Float" => schema_factory(
                description,
                Some(InstanceType::Number),
                None,
                None,
                None,
                None,
            ),
            "Boolean" => schema_factory(
                description,
                Some(InstanceType::Boolean),
                None,
                None,
                None,
                None,
            ),
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
                        Some(InstanceType::Object),
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
                            warn!(name=?named, "custom scalar missing from custom_scalar_map");
                            schema_factory(description, None, None, None, None, None)
                        }
                    } else {
                        warn!(name=?named, "custom scalars aren't currently supported without a custom_scalar_map");
                        schema_factory(None, None, None, None, None, None)
                    }
                } else if let Some(enum_type) = graphql_schema.get_enum(named) {
                    schema_factory(
                        description,
                        Some(InstanceType::String),
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
                    warn!(name=?named, "Type not found in schema");
                    schema_factory(None, None, None, None, None, None)
                }
            }
        },
        Type::NonNullList(list_type) | Type::List(list_type) => {
            let inner_type_schema =
                type_to_schema(description, list_type, graphql_schema, custom_scalar_map);
            schema_factory(
                None,
                Some(InstanceType::Array),
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

impl graphql::Executable for Operation {
    fn persisted_query_id(&self) -> Option<String> {
        self.persisted_query_id.clone()
    }

    fn operation(&self, _input: Value) -> Result<String, McpError> {
        Ok(self.source_text.clone())
    }

    fn variables(&self, input: Value) -> Result<Value, McpError> {
        Ok(input)
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::LazyLock};

    use apollo_compiler::{Schema, parser::Parser, validation::Valid};
    use rmcp::{model::Tool, serde_json};

    use crate::{custom_scalar_map::CustomScalarMap, operations::Operation};

    // Example schema for tests
    static SCHEMA: LazyLock<Valid<Schema>> = LazyLock::new(|| {
        Schema::parse(
            r#"
                type Query { id: String }

                """
                RealCustomScalar exists
                """
                scalar RealCustomScalar
                input RealInputObject {
                    """
                    optional is a input field that is optional
                    """
                    optional: String

                    """
                    required is a input field that is required
                    """
                    required: String!
                }

                """
                the description for the enum
                """
                enum RealEnum {
                    """
                    ENUM_VALUE_1 is a value
                    """
                    ENUM_VALUE_1

                    """
                    ENUM_VALUE_2 is a value
                    """
                    ENUM_VALUE_2
                }
            "#,
            "operation.graphql",
        )
        .expect("schema should parse")
        .validate()
        .expect("schema should be valid")
    });

    #[test]
    fn no_variables() {
        let operation =
            Operation::from_document("query QueryName { id }", &SCHEMA, None, None).unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object"
        }
        "###);
    }

    #[test]
    fn nullable_named_type() {
        let operation =
            Operation::from_document("query QueryName($id: ID) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("string"),
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "string"
            }
          }
        }
        "###);
    }

    #[test]
    fn non_nullable_named_type() {
        let operation =
            Operation::from_document("query QueryName($id: ID!) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "required": Array [
                    String("id"),
                ],
                "properties": Object {
                    "id": Object {
                        "type": String("string"),
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "required": [
            "id"
          ],
          "properties": {
            "id": {
              "type": "string"
            }
          }
        }
        "###);
    }

    #[test]
    fn non_nullable_list_of_nullable_named_type() {
        let operation =
            Operation::from_document("query QueryName($id: [ID]!) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "required": Array [
                    String("id"),
                ],
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "oneOf": Array [
                            Object {
                                "type": String("string"),
                            },
                            Object {
                                "type": String("null"),
                            },
                        ],
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "required": [
            "id"
          ],
          "properties": {
            "id": {
              "type": "array",
              "oneOf": [
                {
                  "type": "string"
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        }
        "###);
    }

    #[test]
    fn non_nullable_list_of_non_nullable_named_type() {
        let operation =
            Operation::from_document("query QueryName($id: [ID!]!) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "required": Array [
                    String("id"),
                ],
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "items": Object {
                            "type": String("string"),
                        },
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "required": [
            "id"
          ],
          "properties": {
            "id": {
              "type": "array",
              "items": {
                "type": "string"
              }
            }
          }
        }
        "###);
    }

    #[test]
    fn nullable_list_of_nullable_named_type() {
        let operation =
            Operation::from_document("query QueryName($id: [ID]) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "oneOf": Array [
                            Object {
                                "type": String("string"),
                            },
                            Object {
                                "type": String("null"),
                            },
                        ],
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "array",
              "oneOf": [
                {
                  "type": "string"
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        }
        "###);
    }

    #[test]
    fn nullable_list_of_non_nullable_named_type() {
        let operation =
            Operation::from_document("query QueryName($id: [ID!]) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "items": Object {
                            "type": String("string"),
                        },
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "array",
              "items": {
                "type": "string"
              }
            }
          }
        }
        "###);
    }

    #[test]
    fn nullable_list_of_nullable_lists_of_nullable_named_types() {
        let operation =
            Operation::from_document("query QueryName($id: [[ID]]) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "oneOf": Array [
                            Object {
                                "type": String("array"),
                                "oneOf": Array [
                                    Object {
                                        "type": String("string"),
                                    },
                                    Object {
                                        "type": String("null"),
                                    },
                                ],
                            },
                            Object {
                                "type": String("null"),
                            },
                        ],
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "array",
              "oneOf": [
                {
                  "type": "array",
                  "oneOf": [
                    {
                      "type": "string"
                    },
                    {
                      "type": "null"
                    }
                  ]
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        }
        "###);
    }

    #[test]
    fn nullable_input_object() {
        let operation = Operation::from_document(
            "query QueryName($id: RealInputObject) { id }",
            &SCHEMA,
            None,
            None,
        )
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("object"),
                        "required": Array [
                            String("required"),
                        ],
                        "properties": Object {
                            "optional": Object {
                                "description": String("optional is a input field that is optional"),
                                "type": String("string"),
                            },
                            "required": Object {
                                "description": String("required is a input field that is required"),
                                "type": String("string"),
                            },
                        },
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "object",
              "required": [
                "required"
              ],
              "properties": {
                "optional": {
                  "description": "optional is a input field that is optional",
                  "type": "string"
                },
                "required": {
                  "description": "required is a input field that is required",
                  "type": "string"
                }
              }
            }
          }
        }
        "###);
    }

    #[test]
    fn non_nullable_enum() {
        let operation = Operation::from_document(
            "query QueryName($id: RealEnum!) { id }",
            &SCHEMA,
            None,
            None,
        )
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "required": Array [
                    String("id"),
                ],
                "properties": Object {
                    "id": Object {
                        "description": String("the description for the enum\n\nValues:\nENUM_VALUE_1: ENUM_VALUE_1 is a value\nENUM_VALUE_2: ENUM_VALUE_2 is a value"),
                        "type": String("string"),
                        "enum": Array [
                            String("ENUM_VALUE_1"),
                            String("ENUM_VALUE_2"),
                        ],
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "required": [
            "id"
          ],
          "properties": {
            "id": {
              "description": "the description for the enum\n\nValues:\nENUM_VALUE_1: ENUM_VALUE_1 is a value\nENUM_VALUE_2: ENUM_VALUE_2 is a value",
              "type": "string",
              "enum": [
                "ENUM_VALUE_1",
                "ENUM_VALUE_2"
              ]
            }
          }
        }
        "###);
    }

    #[test]
    fn multiple_operations_should_error() {
        let operation = Operation::from_document(
            "query QueryName { id } query QueryName { id }",
            &SCHEMA,
            None,
            None,
        );
        insta::assert_debug_snapshot!(operation, @r###"
        Err(
            TooManyOperations(
                2,
            ),
        )
        "###);
    }

    #[test]
    fn unnamed_operations_should_error() {
        let operation = Operation::from_document("query { id }", &SCHEMA, None, None);
        insta::assert_debug_snapshot!(operation, @r###"
        Err(
            MissingName(
                "{ id }",
            ),
        )
        "###);
    }

    #[test]
    fn no_operations_should_error() {
        let operation =
            Operation::from_document("fragment Test on Query { id }", &SCHEMA, None, None);
        insta::assert_debug_snapshot!(operation, @r###"
        Err(
            NoOperations,
        )
        "###);
    }

    #[test]
    fn schema_should_error() {
        let operation = Operation::from_document("type Query { id: String }", &SCHEMA, None, None);
        insta::assert_debug_snapshot!(operation, @r###"
        Err(
            NoOperations,
        )
        "###);
    }

    #[test]
    fn unknown_type_should_be_any() {
        // TODO: should this test that the warning was logged?
        let operation =
            Operation::from_document("query QueryName($id: FakeType) { id }", &SCHEMA, None, None)
                .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {},
                },
            },
        }
        "###);
    }

    #[test]
    fn custom_scalar_without_map_should_be_any() {
        // TODO: should this test that the warning was logged?
        let operation = Operation::from_document(
            "query QueryName($id: RealCustomScalar) { id }",
            &SCHEMA,
            None,
            None,
        )
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {},
                },
            },
        }
        "###);
    }

    #[test]
    fn custom_scalar_with_map_but_not_found_should_error() {
        // TODO: should this test that the warning was logged?
        let operation = Operation::from_document(
            "query QueryName($id: RealCustomScalar) { id }",
            &SCHEMA,
            None,
            Some(&CustomScalarMap::from_str("{}").unwrap()),
        )
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "description": String("RealCustomScalar exists"),
                    },
                },
            },
        }
        "###);
    }

    #[test]
    fn custom_scalar_with_map() {
        let custom_scalar_map =
            CustomScalarMap::from_str("{ \"RealCustomScalar\": { \"type\": \"string\" }}");

        let operation = Operation::from_document(
            "query QueryName($id: RealCustomScalar) { id }",
            &SCHEMA,
            None,
            custom_scalar_map.ok().as_ref(),
        )
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: "The returned value is optional and has type `String`",
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "description": String("RealCustomScalar exists"),
                        "type": String("string"),
                    },
                },
            },
        }
        "###);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "description": "RealCustomScalar exists",
              "type": "string"
            }
          }
        }
        "###);
    }

    #[test]
    fn test_tool_description() {
        const SCHEMA: &str = r#"
        type Query {
          """
          Get a list of A
          """
          a(input: String!): [A]!

          """
          Get a B
          """
          b: B

          """
          Get a Z
          """
          z: Z
        }

        """
        A
        """
        type A {
          c: String
          d: D
        }

        """
        B
        """
        type B {
          d: D
          u: U
        }

        """
        D
        """
        type D {
          e: E
          f: String
          g: String
        }

        """
        E
        """
        enum E {
          """
          one
          """
          ONE
          """
          two
          """
          TWO
        }

        """
        F
        """
        scalar F

        """
        U
        """
        union U = M | W

        """
        M
        """
        type M {
          m: Int
        }

        """
        W
        """
        type W {
          w: Int
        }

        """
        Z
        """
        type Z {
          z: Int
          zz: Int
          zzz: Int
        }
        "#;

        let schema = Parser::new()
            .parse_ast(SCHEMA, "schema.graphql")
            .unwrap()
            .to_schema()
            .unwrap();

        let operation = Operation::from_document(
            r###"
            query GetABZ($state: String!) {
              a(input: $input) {
                d {
                  e
                }
              }
              b {
                d {
                  ...JustF
                }
                u {
                  ... on M {
                    m
                  }
                  ... on W {
                    w
                  }
                }
              }
              z {
                ...JustZZZ
              }
            }

            fragment JustF on D {
              f
            }

            fragment JustZZZ on Z {
              zzz
            }
            "###,
            &schema,
            None,
            None,
        )
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.as_ref(),
            @r###"
        Get a list of A
        The returned value is an array of type `A`
        ---
        Get a B
        The returned value is optional and has type `B`
        ---
        Get a Z
        The returned value is optional and has type `Z`
        ---
        """A"""
        type A {
          d: D
        }

        """D"""
        type D {
          e: E
          f: String
        }

        """E"""
        enum E {
          """one"""
          ONE
          """two"""
          TWO
        }

        """B"""
        type B {
          d: D
          u: U
        }

        """W"""
        type W {
          w: Int
        }

        """M"""
        type M {
          m: Int
        }

        """Z"""
        type Z {
          zzz: Int
        }
        "###
        );
    }

    #[test]
    fn tool_comment_description() {
        let operation = Operation::from_document(
            r###"
            # Overridden tool #description
            query GetABZ($state: String!) {
              b {
                d {
                  f
                }
              }
            }
            "###,
            &SCHEMA,
            None,
            None,
        )
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.as_ref(),
            @r###"Overridden tool #description"###
        );
    }

    #[test]
    fn tool_empty_comment_description() {
        let operation = Operation::from_document(
            r###"
            #

            #
            query GetABZ($state: String!) {
              id
            }
            "###,
            &SCHEMA,
            None,
            None,
        )
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.as_ref(),
            @r###"The returned value is optional and has type `String`"###
        );
    }
}
