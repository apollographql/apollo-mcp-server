use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::{McpError, OperationError};
use crate::event::Event;
use crate::graphql;
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::ast::{Document, OperationType, Selection};
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{
    Name, Node, Schema as GraphqlSchema,
    ast::{Definition, OperationDefinition, Type},
    parser::Parser,
};
use apollo_mcp_registry::files;
use apollo_mcp_registry::platform_api::operation_collections::collection_poller::{
    CollectionSource, OperationData,
};
use apollo_mcp_registry::platform_api::operation_collections::error::CollectionError;
use apollo_mcp_registry::platform_api::operation_collections::event::CollectionEvent;
use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::persisted_queries::event::Event as ManifestEvent;
use futures::{Stream, StreamExt};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{ErrorCode, ToolAnnotations};
use rmcp::schemars::Map;
use rmcp::{
    model::Tool,
    schemars::schema::{
        ArrayValidation, InstanceType, Metadata, ObjectValidation, RootSchema, Schema,
        SchemaObject, SingleOrVec, SubschemaValidation,
    },
    serde_json::{self, Value},
};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

const OPERATION_DOCUMENT_EXTENSION: &str = "graphql";

/// The source of the operations exposed as MCP tools
#[derive(Clone)]
pub enum OperationSource {
    /// GraphQL document files
    Files(Vec<PathBuf>),

    /// Persisted Query manifest
    Manifest(ManifestSource),

    /// Operation collection
    Collection(CollectionSource),

    /// No operations provided
    None,
}

impl OperationSource {
    pub async fn into_stream(self) -> impl Stream<Item = Event> {
        match self {
            OperationSource::Files(paths) => Self::stream_file_changes(paths).boxed(),
            OperationSource::Manifest(manifest_source) => manifest_source
                .into_stream()
                .await
                .map(|event| {
                    let ManifestEvent::UpdateManifest(operations) = event;
                    Event::OperationsUpdated(
                        operations.into_iter().map(RawOperation::from).collect(),
                    )
                })
                .boxed(),
            OperationSource::Collection(collection_source) => collection_source
                .into_stream()
                .map(|event| match event {
                    CollectionEvent::UpdateOperationCollection(operations) => {
                        match operations
                            .iter()
                            .map(RawOperation::try_from)
                            .collect::<Result<Vec<_>, _>>()
                        {
                            Ok(operations) => Event::OperationsUpdated(operations),
                            Err(e) => Event::CollectionError(e),
                        }
                    }
                    CollectionEvent::CollectionError(error) => Event::CollectionError(error),
                })
                .boxed(),
            OperationSource::None => {
                futures::stream::once(async { Event::OperationsUpdated(vec![]) }).boxed()
            }
        }
    }

    fn stream_file_changes(paths: Vec<PathBuf>) -> impl Stream<Item = Event> {
        let path_count = paths.len();
        let state = Arc::new(Mutex::new(HashMap::<PathBuf, Vec<RawOperation>>::new()));
        futures::stream::select_all(paths.into_iter().map(|path| {
            let state = Arc::clone(&state);
            files::watch(path.as_ref())
                .filter_map(move |_| {
                    let path = path.clone();
                    let state = Arc::clone(&state);
                    async move {
                        let mut operations = Vec::new();
                        if path.is_dir() {
                            // Handle a directory
                            if let Ok(entries) = fs::read_dir(&path) {
                                for entry in entries.flatten() {
                                    let entry_path = entry.path();
                                    if entry_path.extension().and_then(|e| e.to_str())
                                        == Some(OPERATION_DOCUMENT_EXTENSION)
                                    {
                                        if let Ok(content) = fs::read_to_string(&entry_path) {
                                            // Be forgiving of empty files in the directory case.
                                            // It likely means a new file was created in an editor,
                                            // but the operation hasn't been written yet.
                                            if !content.trim().is_empty() {
                                                operations.push(RawOperation::from(content));
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            // Handle a single file
                            match fs::read_to_string(&path) {
                                Ok(content) => {
                                    if !content.trim().is_empty() {
                                        operations.push(RawOperation::from(content));
                                    } else {
                                        warn!(?path, "Empty operation file");
                                    }
                                }
                                Err(e) => return Some(Event::OperationError(e)),
                            }
                        }
                        match state.lock() {
                            Ok(mut state) => {
                                state.insert(path.clone(), operations);
                                // All paths send an initial event on startup. To avoid repeated
                                // operation events on startup, wait until all paths have been
                                // loaded, then send a single event with the operations for all
                                // paths.
                                if state.len() == path_count {
                                    Some(Event::OperationsUpdated(
                                        state.values().flatten().cloned().collect::<Vec<_>>(),
                                    ))
                                } else {
                                    None
                                }
                            }
                            Err(_) => Some(Event::OperationError(std::io::Error::other(
                                "State mutex poisoned",
                            ))),
                        }
                    }
                })
                .boxed()
        }))
        .boxed()
    }
}

impl From<ManifestSource> for OperationSource {
    fn from(manifest_source: ManifestSource) -> Self {
        OperationSource::Manifest(manifest_source)
    }
}

impl From<Vec<PathBuf>> for OperationSource {
    fn from(paths: Vec<PathBuf>) -> Self {
        OperationSource::Files(paths)
    }
}

#[derive(clap::ValueEnum, Clone, Default, Debug, Serialize, PartialEq, Copy)]
pub enum MutationMode {
    /// Don't allow any mutations
    #[default]
    None,
    /// Allow explicit mutations, but don't allow the LLM to build them
    Explicit,
    /// Allow the LLM to build mutations
    All,
}

#[derive(Debug, Clone)]
pub struct RawOperation {
    source_text: String,
    persisted_query_id: Option<String>,
    headers: Option<HeaderMap<HeaderValue>>,
    variables: Option<HashMap<String, Value>>,
}

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
        state.end()
    }
}

impl From<String> for RawOperation {
    fn from(source_text: String) -> Self {
        Self {
            source_text,
            persisted_query_id: None,
            headers: None,
            variables: None,
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
        })
    }
}

impl RawOperation {
    pub(crate) fn into_operation(
        self,
        schema: &Valid<apollo_compiler::Schema>,
        custom_scalars: Option<&CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
    ) -> Result<Option<Operation>, OperationError> {
        Operation::from_document(
            self,
            schema,
            custom_scalars,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Operation {
    tool: Tool,
    inner: RawOperation,
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
}

#[allow(clippy::type_complexity)]
pub fn operation_defs(
    source_text: &str,
    allow_mutations: bool,
) -> Result<Option<(Document, Node<OperationDefinition>, Option<String>)>, OperationError> {
    let document = Parser::new()
        .parse_ast(source_text, "operation.graphql")
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
        (None, _) => return Err(OperationError::NoOperations),
        (_, Some(_)) => {
            return Err(OperationError::TooManyOperations(
                2 + operation_defs.count(),
            ));
        }
        (Some(op), None) => op,
    };

    match operation.operation_type {
        OperationType::Subscription => {
            debug!(
                "Skipping subscription operation {}",
                operation_name(&operation)?
            );
            return Ok(None);
        }
        OperationType::Mutation => {
            if !allow_mutations {
                warn!(
                    "Skipping mutation operation {}",
                    operation_name(&operation)?
                );
                return Ok(None);
            }
        }
        OperationType::Query => {}
    }

    Ok(Some((document, operation, comments.map(|c| c.to_string()))))
}

impl Operation {
    pub fn from_document(
        raw_operation: RawOperation,
        graphql_schema: &GraphqlSchema,
        custom_scalar_map: Option<&CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
    ) -> Result<Option<Self>, OperationError> {
        if let Some((document, operation, comments)) = operation_defs(
            &raw_operation.source_text,
            mutation_mode != MutationMode::None,
        )? {
            let operation_name = operation_name(&operation)?;

            let description = Self::tool_description(
                comments,
                &document,
                graphql_schema,
                &operation,
                disable_type_description,
                disable_schema_description,
            );

            let object = serde_json::to_value(get_json_schema(
                &operation,
                graphql_schema,
                custom_scalar_map,
                raw_operation.variables.as_ref(),
            ))?;
            let Value::Object(schema) = object else {
                return Err(OperationError::Internal(
                    "Schemars should have returned an object".to_string(),
                ));
            };

            let tool: Tool = Tool::new(operation_name.clone(), description, schema).annotate(
                ToolAnnotations::new()
                    .read_only(operation.operation_type != OperationType::Mutation),
            );
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
            }))
        } else {
            Ok(None)
        }
    }

    /// Generate a description for an operation based on documentation in the schema
    fn tool_description(
        comments: Option<String>,
        document: &Document,
        graphql_schema: &GraphqlSchema,
        operation_def: &Node<OperationDefinition>,
        disable_type_description: bool,
        disable_schema_description: bool,
    ) -> String {
        let comment_description = comments.and_then(|comments| {
            let content = Regex::new(r"(\n|^)\s*#")
                .ok()?
                .replace_all(comments.as_str(), "$1");
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
                    let mut tree_shaker = SchemaTreeShaker::new(graphql_schema);
                    tree_shaker.retain_operation(operation_def, document, DepthLimit::Unlimited);
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
            "The returned value {}{} `{}`",
            optional, array, type_name
        ));

        lines.join("\n")
    }
}

fn operation_name(operation: &Node<OperationDefinition>) -> Result<String, OperationError> {
    Ok(operation
        .name
        .as_ref()
        .ok_or_else(|| OperationError::MissingName(operation.serialize().no_indent().to_string()))?
        .to_string())
}

fn tool_character_length(tool: &Tool) -> Result<usize, serde_json::Error> {
    let tool_schema_string = serde_json::to_string_pretty(&serde_json::json!(tool.input_schema))?;
    Ok(tool.name.len()
        + tool.description.as_ref().map(|d| d.len()).unwrap_or(0)
        + tool_schema_string.len())
}

fn get_json_schema(
    operation: &Node<OperationDefinition>,
    graphql_schema: &GraphqlSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    variable_overrides: Option<&HashMap<String, Value>>,
) -> RootSchema {
    let mut obj = ObjectValidation::default();
    let mut definitions = Map::new();

    operation.variables.iter().for_each(|variable| {
        let variable_name = variable.name.to_string();
        if !variable_overrides
            .map(|o| o.contains_key(&variable_name))
            .unwrap_or_default()
        {
            let schema = type_to_schema(
                None,
                variable.ty.as_ref(),
                graphql_schema,
                custom_scalar_map,
                &mut definitions,
            );
            obj.properties.insert(variable_name.clone(), schema);
            if variable.ty.is_non_null() {
                obj.required.insert(variable_name);
            }
        }
    });

    RootSchema {
        schema: SchemaObject {
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
            object: Some(Box::new(obj)),
            ..Default::default()
        },
        definitions,
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

fn input_object_description(name: &Name, graphql_schema: &GraphqlSchema) -> Option<String> {
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
    definitions: &mut Map<String, Schema>,
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
                    if !definitions.contains_key(named.as_str()) {
                        definitions
                            .insert(named.to_string(), Schema::Object(SchemaObject::default())); // Insert temporary value into map so any recursive references will not try to also create it.
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
                                    definitions,
                                ),
                            );

                            if field.is_required() {
                                obj.required.insert(name.to_string());
                            }
                        });

                        definitions.insert(
                            named.to_string(),
                            schema_factory(
                                input_object_description(named, graphql_schema),
                                Some(InstanceType::Object),
                                Some(obj),
                                None,
                                None,
                                None,
                            ),
                        );
                    }

                    Schema::Object(SchemaObject {
                        metadata: Some(Box::new(Metadata {
                            description,
                            ..Default::default()
                        })),
                        reference: Some(format!("#/definitions/{}", named)),
                        ..Default::default()
                    })
                } else if graphql_schema.get_scalar(named).is_some() {
                    if !definitions.contains_key(named.as_str()) {
                        let default_description = input_object_description(named, graphql_schema);
                        if let Some(custom_scalar_map) = custom_scalar_map {
                            if let Some(custom_scalar_schema_object) =
                                custom_scalar_map.get(named.as_str())
                            {
                                let mut custom_schema = custom_scalar_schema_object.clone();
                                let mut meta = *custom_schema.metadata.unwrap_or_default();
                                // If description isn't included in custom schema, inject the one from the schema
                                if meta.description.is_none() {
                                    meta.description = default_description;
                                }
                                custom_schema.metadata = Some(Box::new(meta));
                                definitions
                                    .insert(named.to_string(), Schema::Object(custom_schema));
                            } else {
                                warn!(name=?named, "custom scalar missing from custom_scalar_map");
                                definitions.insert(
                                    named.to_string(),
                                    schema_factory(
                                        default_description,
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                    ),
                                );
                            }
                        } else {
                            warn!(name=?named, "custom scalars aren't currently supported without a custom_scalar_map");
                            definitions.insert(
                                named.to_string(),
                                schema_factory(default_description, None, None, None, None, None),
                            );
                        }
                    }
                    Schema::Object(SchemaObject {
                        metadata: Some(Box::new(Metadata {
                            description,
                            ..Default::default()
                        })),
                        reference: Some(format!("#/definitions/{}", named)),
                        ..Default::default()
                    })
                } else if let Some(enum_type) = graphql_schema.get_enum(named) {
                    if !definitions.contains_key(named.as_str()) {
                        definitions.insert(
                            named.to_string(),
                            schema_factory(
                                input_object_description(named, graphql_schema),
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
                            ),
                        );
                    }
                    Schema::Object(SchemaObject {
                        metadata: Some(Box::new(Metadata {
                            description,
                            ..Default::default()
                        })),
                        reference: Some(format!("#/definitions/{}", named)),
                        ..Default::default()
                    })
                } else {
                    warn!(name=?named, "Type not found in schema");
                    schema_factory(None, None, None, None, None, None)
                }
            }
        },
        Type::NonNullList(list_type) | Type::List(list_type) => {
            let inner_type_schema = type_to_schema(
                description,
                list_type,
                graphql_schema,
                custom_scalar_map,
                definitions,
            );
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
        // TODO: id was being overridden, should we be returning? Should this be behind a flag? self.inner.persisted_query_id.clone()
        None
    }

    fn operation(&self, _input: Value) -> Result<String, McpError> {
        Ok(self.inner.source_text.clone())
    }

    fn variables(&self, input_variables: Value) -> Result<Value, McpError> {
        if let Some(raw_variables) = self.inner.variables.as_ref() {
            let mut variables = match input_variables {
                Value::Null => Ok(serde_json::Map::new()),
                Value::Object(obj) => Ok(obj.clone()),
                _ => Err(McpError::new(
                    ErrorCode::INVALID_PARAMS,
                    "Invalid input".to_string(),
                    None,
                )),
            }?;

            raw_variables.iter().try_for_each(|(key, value)| {
                if variables.contains_key(key) {
                    Err(McpError::new(
                        ErrorCode::INVALID_PARAMS,
                        "No such parameter: {key}",
                        None,
                    ))
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr, sync::LazyLock};

    use apollo_compiler::{Schema, parser::Parser, validation::Valid};
    use rmcp::{model::Tool, serde_json};

    use crate::{
        custom_scalar_map::CustomScalarMap,
        operations::{MutationMode, Operation, RawOperation},
    };

    // Example schema for tests
    static SCHEMA: LazyLock<Valid<Schema>> = LazyLock::new(|| {
        Schema::parse(
            r#"
                type Query { id: String enum: RealEnum }
                type Mutation { id: String }

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
    fn subscriptions() {
        assert!(
            Operation::from_document(
                RawOperation {
                    source_text: "subscription SubscriptionName { id }".to_string(),
                    persisted_query_id: None,
                    headers: None,
                    variables: None,
                },
                &SCHEMA,
                None,
                MutationMode::None,
                false,
                false,
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
                },
                &SCHEMA,
                None,
                MutationMode::None,
                false,
                false,
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
            },
            &SCHEMA,
            None,
            MutationMode::Explicit,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        insta::assert_debug_snapshot!(operation, @r###"
        Operation {
            tool: Tool {
                name: "MutationName",
                description: Some(
                    "The returned value is optional and has type `String`",
                ),
                input_schema: {
                    "type": String("object"),
                },
                annotations: Some(
                    ToolAnnotations {
                        title: None,
                        read_only_hint: Some(
                            false,
                        ),
                        destructive_hint: None,
                        idempotent_hint: None,
                        open_world_hint: None,
                    },
                ),
            },
            inner: RawOperation {
                source_text: "mutation MutationName { id }",
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
        }
        "###);
    }

    #[test]
    fn mutation_mode_all() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "mutation MutationName { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::All,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        insta::assert_debug_snapshot!(operation, @r###"
        Operation {
            tool: Tool {
                name: "MutationName",
                description: Some(
                    "The returned value is optional and has type `String`",
                ),
                input_schema: {
                    "type": String("object"),
                },
                annotations: Some(
                    ToolAnnotations {
                        title: None,
                        read_only_hint: Some(
                            false,
                        ),
                        destructive_hint: None,
                        idempotent_hint: None,
                        open_world_hint: None,
                    },
                ),
            },
            inner: RawOperation {
                source_text: "mutation MutationName { id }",
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
        }
        "###);
    }

    #[test]
    fn no_variables() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
            },
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: ID) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: ID!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID]!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID!]!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID]) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [ID!]) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: [[ID]]) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
            RawOperation {
                source_text: "query QueryName($id: RealInputObject) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
        }
        "###);
    }

    #[test]
    fn non_nullable_enum() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealEnum!) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "required": Array [
                    String("id"),
                ],
                "properties": Object {
                    "id": Object {
                        "$ref": String("#/definitions/RealEnum"),
                    },
                },
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
        }
        "###);
    }

    #[test]
    fn multiple_operations_should_error() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName { id } query QueryName { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        );
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
        let operation = Operation::from_document(
            RawOperation {
                source_text: "fragment Test on Query { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        );
        insta::assert_debug_snapshot!(operation, @r###"
        Err(
            NoOperations,
        )
        "###);
    }

    #[test]
    fn schema_should_error() {
        let operation = Operation::from_document(
            RawOperation {
                source_text: "type Query { id: String }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        );
        insta::assert_debug_snapshot!(operation, @r###"
        Err(
            NoOperations,
        )
        "###);
    }

    #[test]
    fn unknown_type_should_be_any() {
        // TODO: should this test that the warning was logged?
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: FakeType) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
            description: Some(
                "The returned value is optional and has type `String`",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "id": Object {},
                },
            },
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
        }
        "###);
    }

    #[test]
    fn custom_scalar_without_map_should_be_any() {
        // TODO: should this test that the warning was logged?
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealCustomScalar) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
        }
        "###);
    }

    #[test]
    fn custom_scalar_with_map_but_not_found_should_error() {
        // TODO: should this test that the warning was logged?
        let operation = Operation::from_document(
            RawOperation {
                source_text: "query QueryName($id: RealCustomScalar) { id }".to_string(),
                persisted_query_id: None,
                headers: None,
                variables: None,
            },
            &SCHEMA,
            Some(&CustomScalarMap::from_str("{}").unwrap()),
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
        }
        "###);
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
            },
            &SCHEMA,
            custom_scalar_map.ok().as_ref(),
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
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
            },
            &schema,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
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
        "###
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
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r###"Overridden tool #description"###
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
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r###"The returned value is optional and has type `String`"###
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
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r###"
                The returned value is optional and has type `String`
                ---
                The returned value is optional and has type `RealEnum`
            "###
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
            },
            &SCHEMA,
            None,
            MutationMode::None,
            true,
            false,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r###"
                """the description for the enum"""
                enum RealEnum {
                  """ENUM_VALUE_1 is a value"""
                  ENUM_VALUE_1
                  """ENUM_VALUE_2 is a value"""
                  ENUM_VALUE_2
                }
            "###
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
            },
            &SCHEMA,
            None,
            MutationMode::None,
            true,
            true,
        )
        .unwrap()
        .unwrap();

        insta::assert_snapshot!(
            operation.tool.description.unwrap(),
            @r###""###
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
        )
        .unwrap()
        .unwrap();

        insta::assert_debug_snapshot!(operation.tool, @r###"
        Tool {
            name: "Test",
            description: Some(
                "",
            ),
            input_schema: {
                "type": String("object"),
                "properties": Object {
                    "filter": Object {
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
        }
        "###);
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
            },
            &SCHEMA,
            None,
            MutationMode::None,
            false,
            false,
        )
        .unwrap()
        .unwrap();
        let tool = Tool::from(operation);

        insta::assert_debug_snapshot!(tool, @r###"
        Tool {
            name: "QueryName",
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
            annotations: Some(
                ToolAnnotations {
                    title: None,
                    read_only_hint: Some(
                        true,
                    ),
                    destructive_hint: None,
                    idempotent_hint: None,
                    open_world_hint: None,
                },
            ),
        }
        "###);
    }
}
