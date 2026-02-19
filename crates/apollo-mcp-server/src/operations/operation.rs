use std::collections::HashMap;

use apollo_compiler::{
    Node, Schema as GraphqlSchema,
    ast::{Definition, Document, OperationDefinition, OperationType, Selection, Type},
    parser::Parser,
    schema::ExtendedType,
};
use http::{HeaderMap, HeaderValue};
use regex::Regex;
use rmcp::model::{Tool, ToolAnnotations};
use schemars::{Schema, json_schema};
use serde::Serialize;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

use crate::{
    custom_scalar_map::CustomScalarMap,
    errors::OperationError,
    graphql::{self, OperationDetails, ValidationError},
    schema_tree_shake::{DepthLimit, SchemaTreeShaker},
};

use super::{MutationMode, RawOperation, schema_walker};

/// A valid GraphQL operation
#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    pub(crate) tool: Tool,
    pub(crate) inner: RawOperation,
    operation_name: String,
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
    pub(crate) fn into_inner(self) -> RawOperation {
        self.inner
    }

    #[tracing::instrument(skip_all, name = "load_tool")]
    pub fn from_document(
        raw_operation: RawOperation,
        graphql_schema: &GraphqlSchema,
        custom_scalar_map: Option<&CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
        enable_output_schema: bool,
    ) -> Result<Option<Self>, OperationError> {
        if let Some((document, operation, comments)) = operation_defs(
            &raw_operation.source_text,
            mutation_mode != MutationMode::None,
            raw_operation.source_path.clone(),
        )? {
            let operation_name = match operation_name(&operation, raw_operation.source_path.clone())
            {
                Ok(name) => name,
                Err(OperationError::MissingName {
                    source_path,
                    operation,
                }) => {
                    if let Some(path) = source_path {
                        warn!("Skipping unnamed operation in {path}: {operation}");
                    } else {
                        warn!("Skipping unnamed operation: {operation}");
                    }
                    return Ok(None);
                }
                Err(e) => return Err(e),
            };
            let variable_description_overrides =
                variable_description_overrides(&raw_operation.source_text, &operation);
            let mut tree_shaker = SchemaTreeShaker::new(graphql_schema);
            tree_shaker.retain_operation(&operation, &document, DepthLimit::Unlimited);

            let description = raw_operation.description.clone().unwrap_or_else(|| {
                Self::tool_description(
                    comments,
                    &mut tree_shaker,
                    graphql_schema,
                    &operation,
                    disable_type_description,
                    disable_schema_description,
                )
            });

            let mut object = serde_json::to_value(get_json_schema(
                &operation,
                tree_shaker.argument_descriptions(),
                &variable_description_overrides,
                graphql_schema,
                custom_scalar_map,
                raw_operation.variables.as_ref(),
            ))?;

            // make sure that the properties field exists since schemas::ObjectValidation is
            // configured to skip empty maps (in the case where there are no input args)
            ensure_properties_exists(&mut object);

            let Value::Object(schema) = object else {
                return Err(OperationError::Internal(
                    "Schemars should have returned an object".to_string(),
                ));
            };

            // Generate output schema from selection set (only if enabled)
            let output_schema = if enable_output_schema {
                if let Some(root_type_name) =
                    graphql_schema.root_operation(operation.operation_type)
                {
                    if let Some(root_type) = graphql_schema.types.get(root_type_name) {
                        let named_fragments: HashMap<
                            String,
                            Node<apollo_compiler::ast::FragmentDefinition>,
                        > = document
                            .definitions
                            .iter()
                            .filter_map(|def| match def {
                                Definition::FragmentDefinition(fragment_def) => {
                                    Some((fragment_def.name.to_string(), fragment_def.clone()))
                                }
                                _ => None,
                            })
                            .collect();

                        serde_json::to_value(schema_walker::selection_set_to_schema(
                            &operation.selection_set,
                            root_type,
                            graphql_schema,
                            custom_scalar_map,
                            &named_fragments,
                        ))
                        .ok()
                        .and_then(|v| match v {
                            Value::Object(obj) => Some(obj),
                            _ => None,
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let mut tool: Tool = Tool::new(operation_name.clone(), description, schema).annotate(
                ToolAnnotations::new()
                    .read_only(operation.operation_type != OperationType::Mutation)
                    .destructive(operation.operation_type == OperationType::Mutation),
            );
            tool.output_schema = output_schema.map(std::sync::Arc::new);
            let character_count = tool_character_length(&tool);
            match character_count {
                Ok(length) => info!(
                    "Tool {} loaded with a character count of {}. Estimated tokens: {}",
                    operation_name,
                    length,
                    length / 4 // We don't know the tokenization algorithm, so we just use 4 characters per token as a rough estimate. https://docs.anthropic.com/en/docs/resources/glossary#tokens
                ),
                Err(_) => info!(
                    "Tool {} loaded with an unknown character count",
                    operation_name
                ),
            }
            Ok(Some(Operation {
                tool,
                inner: raw_operation,
                operation_name,
            }))
        } else {
            Ok(None)
        }
    }

    /// Generate a description for an operation based on documentation in the schema
    #[tracing::instrument(skip(comments, tree_shaker, graphql_schema, operation_def), fields(operation_type = ?operation_def.operation_type, operation_id = ?operation_def.name))]
    fn tool_description(
        comments: Option<String>,
        tree_shaker: &mut SchemaTreeShaker,
        graphql_schema: &GraphqlSchema,
        operation_def: &Node<OperationDefinition>,
        disable_type_description: bool,
        disable_schema_description: bool,
    ) -> String {
        let comment_description = extract_and_format_comments(comments);

        match comment_description {
            Some(description) => description,
            None => {
                // Add the tree-shaken types to the end of the tool description
                let mut lines = vec![];
                if !disable_type_description {
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
                                            .map(|(_, field_definition)| {
                                                field_definition.node.clone()
                                            });

                                        // Add the root field description to the tool description
                                        let field_description = field_definition
                                            .clone()
                                            .and_then(|field| field.description.clone())
                                            .map(|node| node.to_string());

                                        // Add information about the return type
                                        let ty = field_definition.map(|field| field.ty.clone());
                                        let type_description =
                                            ty.as_ref().map(Self::type_description);

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

                    lines.push(descriptions);
                }
                if !disable_schema_description {
                    let shaken_schema =
                        tree_shaker.shaken().unwrap_or_else(|schema| schema.partial);

                    let mut types = shaken_schema
                        .types
                        .iter()
                        .filter(|(_name, extended_type)| {
                            !extended_type.is_built_in()
                                && matches!(
                                    extended_type,
                                    ExtendedType::Object(_)
                                        | ExtendedType::Scalar(_)
                                        | ExtendedType::Enum(_)
                                        | ExtendedType::Interface(_)
                                        | ExtendedType::Union(_)
                                )
                                && graphql_schema
                                    .root_operation(operation_def.operation_type)
                                    .is_none_or(|op_name| extended_type.name() != op_name)
                                && graphql_schema
                                    .root_operation(OperationType::Query)
                                    .is_none_or(|op_name| extended_type.name() != op_name)
                        })
                        .peekable();
                    if types.peek().is_some() {
                        lines.push(String::from("---"));
                    }

                    for ty in types {
                        lines.push(ty.1.serialize().to_string());
                    }
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
            "The returned value {optional}{array} `{type_name}`"
        ));

        lines.join("\n")
    }
}

impl graphql::Executable for Operation {
    fn persisted_query_id(&self) -> Option<String> {
        // TODO: id was being overridden, should we be returning? Should this be behind a flag? self.inner.persisted_query_id.clone()
        None
    }

    fn operation(&self, _input: Value) -> Result<OperationDetails, ValidationError> {
        Ok(OperationDetails {
            query: self.inner.source_text.clone(),
            operation_name: Some(self.operation_name.clone()),
        })
    }

    fn variables(&self, input_variables: Value) -> Result<Value, ValidationError> {
        if let Some(raw_variables) = self.inner.variables.as_ref() {
            let mut variables = match input_variables {
                Value::Null => Ok(serde_json::Map::new()),
                Value::Object(obj) => Ok(obj.clone()),
                _ => Err(ValidationError(
                    "Variables must be a JSON object or null".into(),
                )),
            }?;

            raw_variables.iter().try_for_each(|(key, value)| {
                if variables.contains_key(key) {
                    Err(ValidationError(format!(
                        "Parameter '{key}' conflicts with operation-defined variable"
                    )))
                } else {
                    variables.insert(key.clone(), value.clone());
                    Ok(())
                }
            })?;

            Ok(Value::Object(variables))
        } else {
            Ok(input_variables)
        }
    }

    fn headers(&self, default_headers: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue> {
        match self.inner.headers.as_ref() {
            None => default_headers.clone(),
            Some(raw_headers) if default_headers.is_empty() => raw_headers.clone(),
            Some(raw_headers) => {
                let mut headers = default_headers.clone();
                raw_headers.iter().for_each(|(key, value)| {
                    if headers.contains_key(key) {
                        tracing::debug!(
                            "Header {} has a default value, overwriting with operation value",
                            key
                        );
                    }
                    headers.insert(key, value.clone());
                });
                headers
            }
        }
    }
}

#[allow(clippy::type_complexity)]
#[tracing::instrument(skip_all)]
pub fn operation_defs(
    source_text: &str,
    allow_mutations: bool,
    source_path: Option<String>,
) -> Result<Option<(Document, Node<OperationDefinition>, Option<String>)>, OperationError> {
    let source_path_clone = source_path.clone();
    let document = Parser::new()
        .parse_ast(
            source_text,
            source_path_clone.unwrap_or_else(|| "operation.graphql".to_string()),
        )
        .map_err(|e| OperationError::GraphQLDocument(Box::new(e)))?;
    let mut last_offset: Option<usize> = Some(0);
    let mut operation_defs = document.definitions.clone().into_iter().filter_map(|def| {
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

    let (operation, comments) = match (operation_defs.next(), operation_defs.next()) {
        (None, _) => {
            return Err(OperationError::NoOperations { source_path });
        }
        (_, Some(_)) => {
            return Err(OperationError::TooManyOperations {
                source_path,
                count: 2 + operation_defs.count(),
            });
        }
        (Some(op), None) => op,
    };

    match operation.operation_type {
        OperationType::Subscription => {
            debug!(
                "Skipping subscription operation {}",
                operation_name(&operation, source_path)?
            );
            return Ok(None);
        }
        OperationType::Mutation => {
            if !allow_mutations {
                warn!(
                    "Skipping mutation operation {}",
                    operation_name(&operation, source_path)?
                );
                return Ok(None);
            }
        }
        OperationType::Query => {}
    }

    Ok(Some((document, operation, comments.map(|c| c.to_string()))))
}

pub fn operation_name(
    operation: &Node<OperationDefinition>,
    source_path: Option<String>,
) -> Result<String, OperationError> {
    Ok(operation
        .name
        .as_ref()
        .ok_or_else(|| OperationError::MissingName {
            source_path,
            operation: operation.serialize().no_indent().to_string(),
        })?
        .to_string())
}

#[tracing::instrument(skip_all, fields(operation_type = ?operation_definition.operation_type, operation_id = ?operation_definition.name))]
pub fn variable_description_overrides(
    source_text: &str,
    operation_definition: &Node<OperationDefinition>,
) -> HashMap<String, String> {
    let mut argument_overrides_map: HashMap<String, String> = HashMap::new();
    let mut last_offset = find_opening_parens_offset(source_text, operation_definition);
    operation_definition
        .variables
        .iter()
        .for_each(|v| match v.location() {
            Some(source_span) => {
                let comment = last_offset
                    .map(|start_offset| &source_text[start_offset..source_span.offset()]);

                if let Some(description) = comment.filter(|d| !d.is_empty() && d.contains('#'))
                    && let Some(description) =
                        extract_and_format_comments(Some(description.to_string()))
                {
                    argument_overrides_map.insert(v.name.to_string(), description);
                }

                last_offset = Some(source_span.end_offset());
            }
            None => {
                last_offset = None;
            }
        });

    argument_overrides_map
}

#[tracing::instrument(skip_all, fields(operation_type = ?operation_definition.operation_type, operation_id = ?operation_definition.name))]
pub fn find_opening_parens_offset(
    source_text: &str,
    operation_definition: &Node<OperationDefinition>,
) -> Option<usize> {
    let regex = match Regex::new(r"(?m)^\s*\(") {
        Ok(regex) => regex,
        Err(_) => return None,
    };

    operation_definition
        .name
        .as_ref()
        .and_then(|n| n.location())
        .map(|span| {
            regex
                .find(source_text[span.end_offset()..].as_ref())
                .map(|m| m.start() + m.len() + span.end_offset())
                .unwrap_or(0)
        })
}

pub fn extract_and_format_comments(comments: Option<String>) -> Option<String> {
    comments.and_then(|comments| {
        let content = Regex::new(r"(\n|^)(\s*,*)*#")
            .ok()?
            .replace_all(comments.as_str(), "$1");
        let trimmed = content.trim();

        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn ensure_properties_exists(json_object: &mut Value) {
    if let Some(obj_type) = json_object.get("type")
        && obj_type == "object"
        && let Some(obj_map) = json_object.as_object_mut()
    {
        let props = obj_map
            .entry("properties")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !props.is_object() {
            *props = Value::Object(serde_json::Map::new());
        }
    }
}

fn tool_character_length(tool: &Tool) -> Result<usize, serde_json::Error> {
    let input_schema_len =
        serde_json::to_string_pretty(&serde_json::json!(tool.input_schema))?.len();
    let output_schema_len = match &tool.output_schema {
        Some(schema) => serde_json::to_string_pretty(schema.as_ref())?.len(),
        None => 0,
    };
    Ok(tool.name.len()
        + tool.description.as_ref().map(|d| d.len()).unwrap_or(0)
        + input_schema_len
        + output_schema_len)
}

#[tracing::instrument(skip_all)]
fn get_json_schema(
    operation: &Node<OperationDefinition>,
    schema_argument_descriptions: &HashMap<String, Vec<String>>,
    argument_descriptions_overrides: &HashMap<String, String>,
    graphql_schema: &GraphqlSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    variable_overrides: Option<&HashMap<String, Value>>,
) -> Schema {
    // Default initialize the schema with the bare minimum needed to be a valid object
    let mut schema = json_schema!({"type": "object", "properties": {}});
    let mut definitions = Map::new();

    // TODO: Can this be unwrapped to use `schema_walker::walk` instead? This functionality is doubled
    // in some cases.
    operation.variables.iter().for_each(|variable| {
        let variable_name = variable.name.to_string();
        if !variable_overrides
            .map(|o| o.contains_key(&variable_name))
            .unwrap_or_default()
        {
            // use overridden description if there is one, otherwise use the schema description
            let description = argument_descriptions_overrides
                .get(&variable_name)
                .cloned()
                .or_else(|| {
                    schema_argument_descriptions
                        .get(&variable_name)
                        .filter(|d| !d.is_empty())
                        .map(|d| d.join("#"))
                });

            let nested = schema_walker::type_to_schema(
                variable.ty.as_ref(),
                graphql_schema,
                &mut definitions,
                custom_scalar_map,
                description,
            );
            schema
                .ensure_object()
                .entry("properties")
                .or_insert(Value::Object(Default::default()))
                .as_object_mut()
                .get_or_insert(&mut Map::default())
                .insert(variable_name.clone(), nested.into());

            if variable.ty.is_non_null() {
                schema
                    .ensure_object()
                    .entry("required")
                    .or_insert(serde_json::Value::Array(Vec::new()))
                    .as_array_mut()
                    .get_or_insert(&mut Vec::default())
                    .push(variable_name.into());
            }
        }
    });

    // Add the definitions to the overall schema if needed
    if !definitions.is_empty() {
        schema
            .ensure_object()
            .insert("definitions".to_string(), definitions.into());
    }

    schema
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr as _, sync::LazyLock};

    use apollo_compiler::{Schema, parser::Parser, validation::Valid};
    use rmcp::model::Tool;
    use serde_json::Value;
    use tracing_test::traced_test;

    use crate::{
        custom_scalar_map::CustomScalarMap,
        graphql::Executable as _,
        operations::{MutationMode, Operation, RawOperation, operation::tool_character_length},
    };

    // Example schema for tests
    static SCHEMA: LazyLock<Valid<Schema>> = LazyLock::new(|| {
        Schema::parse(
            r#"
                type Query {
                    id: String
                    enum: RealEnum
                    customQuery(""" id description """ id: ID!, """ a flag """ flag: Boolean): OutputType
                    testOp: OpResponse
                }
                type Mutation {id: String }

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

                type OpResponse {
                  id: String
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

                """
                custom output type
                """
                type OutputType {
                    id: ID!
                }
            "#,
            "operation.graphql",
        )
        .expect("schema should parse")
        .validate()
        .expect("schema should be valid")
    });

    /// Serializes the input to JSON, sorting the object keys
    macro_rules! to_sorted_json {
        ($json:expr) => {{
            let mut j = serde_json::json!($json);
            j.sort_all_objects();

            j
        }};
    }

    #[test]
    fn nullable_named_type() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: ID) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("string"),
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r#"
        {
          "properties": {
            "id": {
              "type": "string"
            }
          },
          "type": "object"
        }
        "#);
    }

    #[test]
    fn non_nullable_named_type() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: ID!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("string"),
                    },
                },
                "required": Array [
                    String("id"),
                ],
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "string"
            }
          },
          "required": [
            "id"
          ]
        }
        "###);
    }

    #[test]
    fn non_nullable_list_of_nullable_named_type() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID]!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "items": Object {
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
                "required": Array [
                    String("id"),
                ],
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r###"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "array",
              "items": {
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
          },
          "required": [
            "id"
          ]
        }
        "###);
    }

    #[test]
    fn non_nullable_list_of_non_nullable_named_type() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID!]!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
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
                "required": Array [
                    String("id"),
                ],
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
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
          },
          "required": [
            "id"
          ]
        }
        "###);
    }

    #[test]
    fn nullable_list_of_nullable_named_type() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID]) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "items": Object {
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
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r#"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "array",
              "items": {
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
        }
        "#);
    }

    #[test]
    fn nullable_list_of_non_nullable_named_type() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID!]) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
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
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r#"
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
        "#);
    }

    #[test]
    fn nullable_list_of_nullable_lists_of_nullable_named_types() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [[ID]]) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "type": String("array"),
                        "items": Object {
                            "oneOf": Array [
                                Object {
                                    "type": String("array"),
                                    "items": Object {
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
                                Object {
                                    "type": String("null"),
                                },
                            ],
                        },
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r#"
        {
          "type": "object",
          "properties": {
            "id": {
              "type": "array",
              "items": {
                "oneOf": [
                  {
                    "type": "array",
                    "items": {
                      "oneOf": [
                        {
                          "type": "string"
                        },
                        {
                          "type": "null"
                        }
                      ]
                    }
                  },
                  {
                    "type": "null"
                  }
                ]
              }
            }
          }
        }
        "#);
    }

    #[test]
    fn nullable_input_object() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealInputObject) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r##"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "$ref": String("#/definitions/RealInputObject"),
                    },
                },
                "definitions": Object {
                    "RealInputObject": Object {
                        "type": String("object"),
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
                        "required": Array [
                            String("required"),
                        ],
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);
    }

    #[test]
    fn non_nullable_enum() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealEnum!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r##"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "$ref": String("#/definitions/RealEnum"),
                    },
                },
                "required": Array [
                    String("id"),
                ],
                "definitions": Object {
                    "RealEnum": Object {
                        "description": String("the description for the enum\n\nValues:\nENUM_VALUE_1: ENUM_VALUE_1 is a value\nENUM_VALUE_2: ENUM_VALUE_2 is a value"),
                        "type": String("string"),
                        "enum": Array [
                            String("ENUM_VALUE_1"),
                            String("ENUM_VALUE_2"),
                        ],
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);
    }

    #[test]
    fn multiple_operations_should_error() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName { id } query QueryName { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: Some("operation.graphql".to_string()),
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        );
        insta::assert_debug_snapshot!(operation, @r#"
        Err(
            TooManyOperations {
                source_path: Some(
                    "operation.graphql",
                ),
                count: 2,
            },
        )
        "#);
    }

    #[test]
    #[traced_test]
    fn unnamed_operations_should_be_skipped() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: Some("operation.graphql".to_string()),
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        );
        assert!(operation.unwrap().is_none());

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| {
                    line.contains("Skipping unnamed operation in operation.graphql: { id }")
                })
                .then_some(())
                .ok_or("Expected warning about unnamed operation in logs".to_string())
        });
    }

    #[test]
    fn no_operations_should_error() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "fragment Test on Query { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: Some("operation.graphql".to_string()),
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        );
        insta::assert_debug_snapshot!(operation, @r#"
        Err(
            NoOperations {
                source_path: Some(
                    "operation.graphql",
                ),
            },
        )
        "#);
    }

    #[test]
    fn schema_should_error() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "type Query { id: String }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        );
        insta::assert_debug_snapshot!(operation, @r"
        Err(
            NoOperations {
                source_path: None,
            },
        )
        ");
    }

    #[test]
    #[traced_test]
    fn unknown_type_should_be_any() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: FakeType) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        // Verify that a warning was logged
        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("Type not found in schema name=\"FakeType\""))
                .then_some(())
                .ok_or("Expected warning about unknown type in logs".to_string())
        });

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {},
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
    }

    #[test]
    #[traced_test]
    fn custom_scalar_without_map_should_be_any() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealCustomScalar) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        // Verify that a warning was logged
        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("custom scalars aren't currently supported without a custom_scalar_map name=\"RealCustomScalar\""))
                .then_some(())
                .ok_or("Expected warning about custom scalar without map in logs".to_string())
        });

        insta::assert_debug_snapshot!(tool, @r##"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "$ref": String("#/definitions/RealCustomScalar"),
                    },
                },
                "definitions": Object {
                    "RealCustomScalar": Object {
                        "description": String("RealCustomScalar exists"),
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);
    }

    #[test]
    #[traced_test]
    fn custom_scalar_with_map_but_not_found_should_error() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealCustomScalar) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            Some(&CustomScalarMap::from_str("{}").unwrap()),
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        // Verify that a warning was logged
        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| {
                    line.contains(
                        "custom scalar missing from custom_scalar_map name=\"RealCustomScalar\"",
                    )
                })
                .then_some(())
                .ok_or("Expected warning about custom scalar missing in logs".to_string())
        });

        insta::assert_debug_snapshot!(tool, @r##"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "$ref": String("#/definitions/RealCustomScalar"),
                    },
                },
                "definitions": Object {
                    "RealCustomScalar": Object {
                        "description": String("RealCustomScalar exists"),
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);
    }

    #[test]
    fn custom_scalar_with_map() {
        let custom_scalar_map =
            CustomScalarMap::from_str("{ \"RealCustomScalar\": { \"type\": \"string\" }}");

        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealCustomScalar) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            custom_scalar_map.ok().as_ref(),
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r##"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {
                        "$ref": String("#/definitions/RealCustomScalar"),
                    },
                },
                "definitions": Object {
                    "RealCustomScalar": Object {
                        "description": String("RealCustomScalar exists"),
                        "type": String("string"),
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);
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

        let document = Parser::new().parse_ast(SCHEMA, "schema.graphql").unwrap();
        let schema = document.to_schema().unwrap();

        let operation = Operation::from_document(
            RawOperation {
                source_text: r###"
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
            "###
                .to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &schema,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r#"
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

        """B"""
        type B {
          d: D
          u: U
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

        """U"""
        union U = M | W

        """M"""
        type M {
          m: Int
        }

        """W"""
        type W {
          w: Int
        }

        """Z"""
        type Z {
          zzz: Int
        }
        "#
        );
    }

    #[test]
    fn tool_comment_description() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: r###"
            # Overridden tool #description
            query GetABZ($state: String!) {
              b {
                d {
                  f
                }
              }
            }
            "###
                .to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @"Overridden tool #description"
        );
    }

    #[test]
    fn tool_empty_comment_description() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: r###"
            #

            #
            query GetABZ($state: String!) {
              id
            }
            "###
                .to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @"The returned value is optional and has type `String`"
        );
    }

    #[test]
    fn no_schema_description() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: r###"query GetABZ($state: String!) { id enum }"###.to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            true,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r"
        The returned value is optional and has type `String`
        ---
        The returned value is optional and has type `RealEnum`
        "
        );
    }

    #[test]
    fn no_type_description() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: r###"query GetABZ($state: String!) { id enum }"###.to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            true,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r#"
        ---
        """the description for the enum"""
        enum RealEnum {
          """ENUM_VALUE_1 is a value"""
          ENUM_VALUE_1
          """ENUM_VALUE_2 is a value"""
          ENUM_VALUE_2
        }
        "#
        );
    }

    #[test]
    fn no_type_description_or_schema_description() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: r###"query GetABZ($state: String!) { id enum }"###.to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            true,
            true,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @""
        );
    }

    #[test]
    fn recursive_inputs() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: r###"query Test($filter: Filter){
                field(filter: $filter) {
                    id
                }
            }"###
                    .to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &Schema::parse(
                r#"
                """the filter input"""
                input Filter {
                """the filter.field field"""
                    field: String
                    """the filter.filter field"""
                    filter: Filter
                }
                type Query {
                """the Query.field field"""
                  field(
                    """the filter argument"""
                    filter: Filter
                  ): String
                }
            "#,
                "operation.graphql",
            )
            .unwrap(),
            None,
            MutationMode::None,
            true,
            true,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_debug_snapshot!(operation.tool, @r##"
        Tool {
            name: "Test",
            title: None,
            description: Some(
                "",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "filter": Object {
                        "description": String("the filter argument"),
                        "$ref": String("#/definitions/Filter"),
                    },
                },
                "definitions": Object {
                    "Filter": Object {
                        "description": String("the filter input"),
                        "type": String("object"),
                        "properties": Object {
                            "field": Object {
                                "description": String("the filter.field field"),
                                "type": String("string"),
                            },
                            "filter": Object {
                                "description": String("the filter.filter field"),
                                "$ref": String("#/definitions/Filter"),
                            },
                        },
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "field": Object {
                                    "description": String("the Query.field field"),
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);
    }

    #[test]
    fn with_variable_overrides() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: ID, $name: String) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: Some(HashMap::from([(
                    "id".to_string(),
                    serde_json::Value::String("v".to_string()),
                )])),
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "name": Object {
                        "type": String("string"),
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
    }

    #[test]
    fn input_schema_includes_variable_descriptions() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($idArg: ID) { customQuery(id: $idArg) { id } }"
                    .to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {
            "idArg": {
              "description": "id description",
              "type": "string"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn input_schema_includes_joined_variable_descriptions_if_multiple() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($idArg: ID, $flag: Boolean) { customQuery(id: $idArg, flag: $flag) { id @skip(if: $flag) } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
            .unwrap()
            .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {
            "flag": {
              "description": "Skipped when true.#a flag",
              "type": "boolean"
            },
            "idArg": {
              "description": "id description",
              "type": "string"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn input_schema_includes_directive_variable_descriptions() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($idArg: ID, $skipArg: Boolean) { customQuery(id: $idArg) { id @skip(if: $skipArg) } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
            .unwrap()
            .unwrap();
        let tool = Tool::from(operation);

        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r#"
        {
          "type": "object",
          "properties": {
            "idArg": {
              "description": "id description",
              "type": "string"
            },
            "skipArg": {
              "description": "Skipped when true.",
              "type": "boolean"
            }
          }
        }
        "#);
    }

    #[test]
    fn test_operation_name_with_named_query() {
        let source_text = "query GetUser($id: ID!) { user(id: $id) { name email } }";
        let raw_op = RawOperation {
            source_text: source_text.to_string(),
            persisted_query_id: None,
            headers: None,
            variables: None,
            source_path: None,
            description: None,
        };
        let operation = Operation::from_document(
            raw_op,
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        let op_details = operation.operation(Value::Null).unwrap();
        assert_eq!(op_details.operation_name, Some(String::from("GetUser")));
    }

    #[test]
    fn test_operation_name_with_named_mutation() {
        let source_text =
            "mutation CreateUser($input: UserInput!) { createUser(input: $input) { id name } }";
        let raw_op = RawOperation {
            source_text: source_text.to_string(),
            persisted_query_id: None,
            headers: None,
            variables: None,
            source_path: None,
            description: None,
        };
        let operation = Operation::from_document(
            raw_op,
            &SCHEMA,
            None,
            MutationMode::Explicit,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        let op_details = operation.operation(Value::Null).unwrap();
        assert_eq!(op_details.operation_name, Some(String::from("CreateUser")));
    }

    #[test]
    fn operation_variable_comments_override_schema_descriptions() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "# operation description\nquery QueryName(# id comment override\n$idArg: ID) { customQuery(id: $idArg) { id } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
            .unwrap()
            .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {
            "idArg": {
              "description": "id comment override",
              "type": "string"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn operation_variable_comment_override_supports_multiline_comments() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "# operation description\nquery QueryName(# id comment override\n # multi-line comment \n$idArg: ID) { customQuery(id: $idArg) { id } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
            .unwrap()
            .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {
            "idArg": {
              "description": "id comment override\n multi-line comment",
              "type": "string"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn comment_with_parens_has_comments_extracted_correctly() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName # a comment (with parens)\n(# id comment override\n # multi-line comment \n$idArg: ID) { customQuery(id: $idArg) { id } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
            .unwrap()
            .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {
            "idArg": {
              "description": "id comment override\n multi-line comment",
              "type": "string"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn multiline_comment_with_odd_spacing_and_parens_has_comments_extracted_correctly() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "#  operation comment\n\nquery QueryName # a comment \n#     extra space\n\n\n#  blank lines (with parens)\n\n# another (paren)\n(# id comment override\n # multi-line comment \n$idArg: ID\n, \n# a flag\n$flag: Boolean) { customQuery(id: $idArg, skip: $flag) { id } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
            .unwrap()
            .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {
            "flag": {
              "description": "a flag",
              "type": "boolean"
            },
            "idArg": {
              "description": "id comment override\n multi-line comment",
              "type": "string"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn operation_with_no_variables_is_handled_properly() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName { customQuery(id: \"123\") { id } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {},
          "type": "object"
        }
        "###);
    }

    #[test]
    fn commas_between_variables_are_ignored() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName(# id arg\n $idArg: ID,,\n,,\n # a flag\n $flag: Boolean,  ,,) { customQuery(id: $idArg, flag: $flag) { id } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
            .unwrap()
            .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "properties": {
            "flag": {
              "description": "a flag",
              "type": "boolean"
            },
            "idArg": {
              "description": "id arg",
              "type": "string"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn input_schema_include_properties_field_even_when_operation_has_no_input_args() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query TestOp { testOp { id } }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r#"
        {
          "properties": {},
          "type": "object"
        }
        "#);
    }

    #[test]
    fn nullable_list_of_nullable_input_objects() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($objects: [RealInputObject]) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r##"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "objects": Object {
                        "type": String("array"),
                        "items": Object {
                            "oneOf": Array [
                                Object {
                                    "$ref": String("#/definitions/RealInputObject"),
                                },
                                Object {
                                    "type": String("null"),
                                },
                            ],
                        },
                    },
                },
                "definitions": Object {
                    "RealInputObject": Object {
                        "type": String("object"),
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
                        "required": Array [
                            String("required"),
                        ],
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "definitions": {
            "RealInputObject": {
              "properties": {
                "optional": {
                  "description": "optional is a input field that is optional",
                  "type": "string"
                },
                "required": {
                  "description": "required is a input field that is required",
                  "type": "string"
                }
              },
              "required": [
                "required"
              ],
              "type": "object"
            }
          },
          "properties": {
            "objects": {
              "items": {
                "oneOf": [
                  {
                    "$ref": "#/definitions/RealInputObject"
                  },
                  {
                    "type": "null"
                  }
                ]
              },
              "type": "array"
            }
          },
          "type": "object"
        }
        "###);
    }

    #[test]
    fn non_nullable_list_of_non_nullable_input_objects() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($objects: [RealInputObject!]!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r##"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "objects": Object {
                        "type": String("array"),
                        "items": Object {
                            "$ref": String("#/definitions/RealInputObject"),
                        },
                    },
                },
                "required": Array [
                    String("objects"),
                ],
                "definitions": Object {
                    "RealInputObject": Object {
                        "type": String("object"),
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
                        "required": Array [
                            String("required"),
                        ],
                    },
                },
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "##);

        let json = to_sorted_json!(tool.input_schema);
        insta::assert_snapshot!(serde_json::to_string_pretty(&json).unwrap(), @r###"
        {
          "definitions": {
            "RealInputObject": {
              "properties": {
                "optional": {
                  "description": "optional is a input field that is optional",
                  "type": "string"
                },
                "required": {
                  "description": "required is a input field that is required",
                  "type": "string"
                }
              },
              "required": [
                "required"
              ],
              "type": "object"
            }
          },
          "properties": {
            "objects": {
              "items": {
                "$ref": "#/definitions/RealInputObject"
              },
              "type": "array"
            }
          },
          "required": [
            "objects"
          ],
          "type": "object"
        }
        "###);
    }

    #[test]
    fn subscriptions() {
        assert!(
            Operation::from_document(
                RawOperation {
                    source_text: "subscription SubscriptionName { id }".to_string(),
                    persisted_query_id: None,
                    headers: None,
                    variables: None,
                    source_path: None,
                    description: None,
                },
                &SCHEMA,
                None,
                MutationMode::None,
                false,
                false,
                true,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn mutation_mode_none() {
        assert!(
            Operation::from_document(
                RawOperation {
                    source_text: "mutation MutationName { id }".to_string(),
                    persisted_query_id: None,
                    headers: None,
                    variables: None,
                    source_path: None,
                    description: None,
                },
                &SCHEMA,
                None,
                MutationMode::None,
                false,
                false,
                true,
            )
            .ok()
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn mutation_mode_explicit() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "mutation MutationName { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::Explicit,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_debug_snapshot!(operation, @r#"
        Operation {
            tool: Tool {
                name: "MutationName",
                title: None,
                description: Some(
                    "The returned value is optional and has type `String`",
                ),
                input_schema: {
                    "type": String("object"),
                    "properties": Object {},
                },
                output_schema: Some(
                    {
                        "type": String("object"),
                        "properties": Object {
                            "data": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "id": Object {
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
                            "errors": Object {
                                "type": String("array"),
                                "items": Object {
                                    "type": String("object"),
                                    "properties": Object {
                                        "message": Object {
                                            "type": String("string"),
                                        },
                                        "locations": Object {
                                            "type": String("array"),
                                            "items": Object {
                                                "type": String("object"),
                                                "properties": Object {
                                                    "line": Object {
                                                        "type": String("integer"),
                                                    },
                                                    "column": Object {
                                                        "type": String("integer"),
                                                    },
                                                },
                                            },
                                        },
                                        "path": Object {
                                            "type": String("array"),
                                            "items": Object {
                                                "oneOf": Array [
                                                    Object {
                                                        "type": String("string"),
                                                    },
                                                    Object {
                                                        "type": String("integer"),
                                                    },
                                                ],
                                            },
                                        },
                                        "extensions": Object {
                                            "type": String("object"),
                                        },
                                    },
                                    "required": Array [
                                        String("message"),
                                    ],
                                },
                            },
                            "extensions": Object {
                                "type": String("object"),
                            },
                        },
                    },
                ),
                annotations: Some(
                    ToolAnnotations {
                        title: None,
                        read_only_hint: Some(
                            false,
                        ),
                        destructive_hint: Some(
                            true,
                        ),
                        idempotent_hint: None,
                        open_world_hint: None,
                    },
                ),
                execution: None,
                icons: None,
                meta: None,
            },
            inner: RawOperation {
                source_text: "mutation MutationName { id }",
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            operation_name: "MutationName",
        }
        "#);
    }

    #[test]
    fn mutation_mode_all() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "mutation MutationName { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_debug_snapshot!(operation, @r#"
        Operation {
            tool: Tool {
                name: "MutationName",
                title: None,
                description: Some(
                    "The returned value is optional and has type `String`",
                ),
                input_schema: {
                    "type": String("object"),
                    "properties": Object {},
                },
                output_schema: Some(
                    {
                        "type": String("object"),
                        "properties": Object {
                            "data": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "id": Object {
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
                            "errors": Object {
                                "type": String("array"),
                                "items": Object {
                                    "type": String("object"),
                                    "properties": Object {
                                        "message": Object {
                                            "type": String("string"),
                                        },
                                        "locations": Object {
                                            "type": String("array"),
                                            "items": Object {
                                                "type": String("object"),
                                                "properties": Object {
                                                    "line": Object {
                                                        "type": String("integer"),
                                                    },
                                                    "column": Object {
                                                        "type": String("integer"),
                                                    },
                                                },
                                            },
                                        },
                                        "path": Object {
                                            "type": String("array"),
                                            "items": Object {
                                                "oneOf": Array [
                                                    Object {
                                                        "type": String("string"),
                                                    },
                                                    Object {
                                                        "type": String("integer"),
                                                    },
                                                ],
                                            },
                                        },
                                        "extensions": Object {
                                            "type": String("object"),
                                        },
                                    },
                                    "required": Array [
                                        String("message"),
                                    ],
                                },
                            },
                            "extensions": Object {
                                "type": String("object"),
                            },
                        },
                    },
                ),
                annotations: Some(
                    ToolAnnotations {
                        title: None,
                        read_only_hint: Some(
                            false,
                        ),
                        destructive_hint: Some(
                            true,
                        ),
                        idempotent_hint: None,
                        open_world_hint: None,
                    },
                ),
                execution: None,
                icons: None,
                meta: None,
            },
            inner: RawOperation {
                source_text: "mutation MutationName { id }",
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            operation_name: "MutationName",
        }
        "#);
    }

    #[test]
    fn no_variables() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r#"
        Tool {
            name: "QueryName",
            title: None,
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {},
            },
            output_schema: Some(
                {
                    "type": String("object"),
                    "properties": Object {
                        "data": Object {
                            "type": String("object"),
                            "properties": Object {
                                "id": Object {
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
                        "errors": Object {
                            "type": String("array"),
                            "items": Object {
                                "type": String("object"),
                                "properties": Object {
                                    "message": Object {
                                        "type": String("string"),
                                    },
                                    "locations": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "type": String("object"),
                                            "properties": Object {
                                                "line": Object {
                                                    "type": String("integer"),
                                                },
                                                "column": Object {
                                                    "type": String("integer"),
                                                },
                                            },
                                        },
                                    },
                                    "path": Object {
                                        "type": String("array"),
                                        "items": Object {
                                            "oneOf": Array [
                                                Object {
                                                    "type": String("string"),
                                                },
                                                Object {
                                                    "type": String("integer"),
                                                },
                                            ],
                                        },
                                    },
                                    "extensions": Object {
                                        "type": String("object"),
                                    },
                                },
                                "required": Array [
                                    String("message"),
                                ],
                            },
                        },
                        "extensions": Object {
                            "type": String("object"),
                        },
                    },
                },
            ),
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: Some(
                        false,
                    ),
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
            execution: None,
            icons: None,
            meta: None,
        }
        "#);
        insta::assert_snapshot!(serde_json::to_string_pretty(&serde_json::json!(tool.input_schema)).unwrap(), @r#"
        {
          "type": "object",
          "properties": {}
        }
        "#);
    }

    #[test]
    fn tool_character_length_without_output_schema() {
        use serde_json::Map;

        let mut input_schema = Map::new();
        input_schema.insert("type".to_string(), serde_json::json!("object"));
        input_schema.insert(
            "properties".to_string(),
            serde_json::json!({ "id": { "type": "string" } }),
        );

        let tool = Tool::new("test_tool", "A test tool description", input_schema.clone());

        let length = tool_character_length(&tool).unwrap();

        let expected_input_schema_len =
            serde_json::to_string_pretty(&serde_json::json!(input_schema))
                .unwrap()
                .len();

        assert_eq!(
            length,
            "test_tool".len() + "A test tool description".len() + expected_input_schema_len
        );
    }

    #[test]
    fn tool_character_length_with_output_schema() {
        use serde_json::Map;

        let mut input_schema = Map::new();
        input_schema.insert("type".to_string(), serde_json::json!("object"));
        input_schema.insert(
            "properties".to_string(),
            serde_json::json!({ "id": { "type": "string" } }),
        );

        let mut tool = Tool::new("test_tool", "A test tool description", input_schema.clone());

        let mut output_schema = Map::new();
        output_schema.insert("type".to_string(), serde_json::json!("object"));
        output_schema.insert(
            "properties".to_string(),
            serde_json::json!({
                "data": {
                    "type": "object",
                    "properties": {
                        "result": { "type": "string" }
                    }
                }
            }),
        );
        tool.output_schema = Some(std::sync::Arc::new(output_schema.clone()));

        let length = tool_character_length(&tool).unwrap();

        let expected_input_schema_len =
            serde_json::to_string_pretty(&serde_json::json!(input_schema))
                .unwrap()
                .len();

        let expected_output_schema_len =
            serde_json::to_string_pretty(&serde_json::json!(output_schema))
                .unwrap()
                .len();

        assert_eq!(
            length,
            "test_tool".len()
                + "A test tool description".len()
                + expected_input_schema_len
                + expected_output_schema_len
        );
    }

    #[test]
    fn tool_character_length_output_schema_adds_to_total() {
        use serde_json::Map;

        let mut input_schema = Map::new();
        input_schema.insert("type".to_string(), serde_json::json!("object"));

        let tool_without = Tool::new("test_tool", "A test tool description", input_schema.clone());

        let mut tool_with = tool_without.clone();
        let mut output_schema = Map::new();
        output_schema.insert("type".to_string(), serde_json::json!("object"));
        output_schema.insert(
            "properties".to_string(),
            serde_json::json!({ "data": { "type": "string" } }),
        );
        tool_with.output_schema = Some(std::sync::Arc::new(output_schema.clone()));

        let length_without = tool_character_length(&tool_without).unwrap();
        let length_with = tool_character_length(&tool_with).unwrap();

        let expected_output_schema_len =
            serde_json::to_string_pretty(&serde_json::json!(output_schema))
                .unwrap()
                .len();

        assert_eq!(length_with - length_without, expected_output_schema_len);
    }

    #[test]
    fn explicit_description_overrides_auto_generated() {
        let explicit_desc = "My custom tool description from PQ manifest";
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: ID) { id }".to_string(),
                persisted_query_id: Some("hash123".to_string()),
                headers: None,
                variables: None,
                source_path: None,
                description: Some(explicit_desc.to_string()),
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            operation.tool.description.as_deref(),
            Some(explicit_desc),
            "tool description should use the explicit description from RawOperation"
        );
    }

    #[test]
    fn no_explicit_description_falls_back_to_auto_generated() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: ID) { id }".to_string(),
                persisted_query_id: Some("hash123".to_string()),
                headers: None,
                variables: None,
                source_path: None,
                description: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        assert!(
            operation.tool.description.is_some(),
            "tool description should be auto-generated when no explicit description"
        );
        assert_ne!(
            operation.tool.description.as_deref(),
            Some(""),
            "auto-generated description should not be empty"
        );
    }

    #[test]
    fn explicit_description_overrides_comments() {
        let explicit_desc = "Override from manifest";
        let operation = Operation::from_document(
            RawOperation {
                source_text: "# Comment-based description\nquery QueryName($id: ID) { id }"
                    .to_string(),
                persisted_query_id: Some("hash123".to_string()),
                headers: None,
                variables: None,
                source_path: None,
                description: Some(explicit_desc.to_string()),
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            operation.tool.description.as_deref(),
            Some(explicit_desc),
            "explicit description should take priority over comment-based description"
        );
    }
}
