use core::fmt;
use std::fmt::Display;
use std::path::Path;
use std::{fs::read_to_string, sync::Arc};

use apollo_compiler::{Schema, validation::Valid};
use rmcp::model::{Meta, RawResource, Resource, Tool};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tracing::debug;
use url::Url;

use crate::{
    custom_scalar_map::CustomScalarMap,
    operations::{MutationMode, Operation, RawOperation},
};

mod execution;

pub(crate) use execution::find_and_execute_app;

/// An app, which consists of a tool and a resource to be used together.
#[derive(Clone, Debug)]
pub(crate) struct App {
    pub(crate) name: String,
    /// The HTML resource that serves as the app's UI
    pub(crate) resource: AppResource,
    /// Any CSP settings to apply to the resource
    pub(crate) csp_settings: Option<CSPSettings>,
    /// The URI of the app's resource
    pub(crate) uri: Url,
    /// Entrypoint tools for this app
    pub(crate) tools: Vec<AppTool>,
    /// Any operations that should _always_ be executed for any of the tools (after the initial tool operation)
    pub(crate) prefetch_operations: Vec<PrefetchOperation>,
}

#[derive(Clone, Debug)]
pub(crate) enum AppResource {
    Local(String),
    Remote(Url),
}

/// An MCP tool which serves as an entrypoint for an app.
#[derive(Clone, Debug)]
pub(crate) struct AppTool {
    /// The GraphQL operation that's executed when the tool is called. Its data is injected into the UI
    pub(crate) operation: Arc<Operation>,
    /// The MCP tool definition
    pub(crate) tool: Tool,
}

/// An operation that should be executed for every invocation of an app.
#[derive(Clone, Debug)]
pub(crate) struct PrefetchOperation {
    /// The operation to execute
    pub(crate) operation: Arc<Operation>,
    /// A unique ID for the operation that the UI will use to look up its data
    pub(crate) prefetch_id: String,
}

impl App {
    pub(crate) fn resource(&self) -> Resource {
        Resource::new(
            RawResource {
                name: self.name.clone(),
                uri: self.uri.to_string(),
                mime_type: Some("text/html+skybridge".to_string()),
                // TODO: load all this from a manifest file
                title: None,
                description: None,
                icons: None,
                size: None,
            },
            None,
        )
    }
}

const MANIFEST_FILE_NAME: &str = ".application-manifest.json";

pub(crate) fn load_from_path(
    path: &Path,
    schema: &Valid<Schema>,
    custom_scalar_map: Option<&CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
    enable_output_schema: bool,
) -> Result<Vec<App>, String> {
    let Ok(apps_dir) = path.read_dir() else {
        return Ok(Vec::new());
    };

    let mut apps = Vec::new();
    for app_dir in apps_dir {
        let app_dir = match app_dir {
            Ok(app_dir) => app_dir,
            Err(err) => {
                debug!("Failed to read app directory, ignoring: {}", err);
                continue;
            }
        };
        let path = app_dir.path();
        if !path.is_dir() {
            debug!("{} is not a directory, ignoring", path.to_string_lossy());
            continue;
        }

        let Ok(manifest) = read_to_string(path.join(MANIFEST_FILE_NAME)) else {
            debug!(
                "No manifest file found in {}, ignoring",
                path.to_string_lossy()
            );
            continue;
        };

        let manifest: Manifest = serde_json::from_str(&manifest).map_err(|err| {
            format!(
                "Failed to parse manifest from {}: {}",
                path.to_string_lossy(),
                err
            )
        })?;

        let name = manifest
            .name
            .unwrap_or_else(|| app_dir.file_name().to_string_lossy().to_string());

        let uri_string = format!("ui://widget/{name}#{}", manifest.hash);
        let uri = Url::parse(&uri_string)
            .map_err(|err| format!("Failed to create a URI for resource {uri_string}: {err}",))?;

        let mut meta = Meta::new();
        meta.insert("openai/outputTemplate".to_string(), uri.to_string().into());
        meta.insert("openai/widgetAccessible".to_string(), true.into());

        if let Some(labels) = manifest.labels {
            if let Some(tool_invocation_invoking) = labels.tool_invocation_invoking {
                meta.insert(
                    "openai/toolInvocation/invoking".into(),
                    tool_invocation_invoking.into(),
                );
            }

            if let Some(tool_invocation_invoked) = labels.tool_invocation_invoked {
                meta.insert(
                    "openai/toolInvocation/invoked".into(),
                    tool_invocation_invoked.into(),
                );
            }
        }

        let mut prefetch_operations = Vec::new();
        let mut tools = Vec::new();

        for operation_def in manifest.operations {
            let raw = RawOperation::from((operation_def.body, path.to_str().map(String::from)));
            let operation = match Operation::from_document(
                raw,
                schema,
                custom_scalar_map,
                mutation_mode,
                disable_type_description,
                disable_schema_description,
                enable_output_schema,
            ) {
                Err(err) => {
                    return Err(format!(
                        "Failed to parse operation from {path}: {err}",
                        path = path.to_string_lossy()
                    ));
                }
                Ok(None) => {
                    return Err(format!(
                        "Failed parsing tools: No operation in {path}",
                        path = path.to_string_lossy()
                    ));
                }
                Ok(Some(op)) => Arc::new(op),
            };

            for tool in operation_def.tools {
                let mut meta = meta.clone();

                // Allow overriding the labels per tool
                if let Some(labels) = tool.labels {
                    if let Some(tool_invocation_invoking) = labels.tool_invocation_invoking {
                        meta.insert(
                            "openai/toolInvocation/invoking".into(),
                            tool_invocation_invoking.into(),
                        );
                    }

                    if let Some(tool_invocation_invoked) = labels.tool_invocation_invoked {
                        meta.insert(
                            "openai/toolInvocation/invoked".into(),
                            tool_invocation_invoked.into(),
                        );
                    }
                }

                let tool = Tool {
                    name: format!("{name}--{}", tool.name).into(),
                    meta: Some(meta.clone()),
                    description: Some(
                        if let Some(app_description) = manifest.description.clone() {
                            format!("{} {}", app_description, tool.description).into()
                        } else {
                            tool.description.into()
                        },
                    ),
                    input_schema: if let Some(extra_inputs) = tool.extra_inputs {
                        let mut merged = operation.tool.input_schema.as_ref().clone();
                        merge_inputs(&mut merged, extra_inputs)?;
                        Arc::new(merged)
                    } else {
                        operation.tool.input_schema.clone()
                    },
                    title: operation.tool.title.clone(),
                    output_schema: operation.tool.output_schema.clone(),
                    annotations: operation.tool.annotations.clone(),
                    icons: operation.tool.icons.clone(),
                };

                tools.push(AppTool {
                    operation: operation.clone(),
                    tool,
                })
            }

            if let Some(prefetch_id) = operation_def.prefetch_id {
                prefetch_operations.push(PrefetchOperation {
                    prefetch_id,
                    operation,
                });
            }
        }

        let resource = if manifest.resource.starts_with("http://")
            || manifest.resource.starts_with("https://")
        {
            let url = Url::parse(&manifest.resource).map_err(|err| {
                format!("Failed to parse resource URL {}: {err}", manifest.resource)
            })?;
            AppResource::Remote(url)
        } else {
            let resource_path = path.join(&manifest.resource);
            let contents = read_to_string(&resource_path).map_err(|err| {
                format!(
                    "Failed to read resource from {resource_path}: {err}",
                    resource_path = resource_path.to_string_lossy(),
                )
            })?;
            AppResource::Local(contents)
        };

        apps.push(App {
            name,
            uri,
            resource,
            csp_settings: manifest.csp,
            tools,
            prefetch_operations,
        });
    }
    Ok(apps)
}

fn merge_inputs(
    orig: &mut Map<String, Value>,
    extra: Vec<ExtraInputDefinition>,
) -> Result<(), String> {
    let mut properties = match orig.remove("properties") {
        Some(Value::Object(props)) => props,
        _ => Map::default(),
    };

    let mut required = match orig.remove("required") {
        Some(Value::Array(req)) => req,
        _ => Vec::default(),
    };

    for extra_input in extra {
        if properties.contains_key(&extra_input.name) {
            return Err(format!(
                "Extra input with name '{}' failed to process because another input with this name was already processed. Make sure your extra_input names are unique, both from each other and any graphql variables you may have.",
                extra_input.name
            ));
        }

        if extra_input.required {
            let value = Value::String(extra_input.name.clone());
            if !required.contains(&value) {
                required.push(value);
            }
        }

        properties.insert(
            extra_input.name,
            json!({
                "description": extra_input.description,
                "type": extra_input.value_type.to_string()
            }),
        );
    }

    orig.insert("properties".to_string(), Value::Object(properties));
    orig.insert("required".to_string(), Value::Array(required));

    Ok(())
}

#[derive(Clone, Deserialize)]
struct Manifest {
    hash: String,
    operations: Vec<OperationDefinition>,
    resource: String,
    name: Option<String>,
    description: Option<String>,
    csp: Option<CSPSettings>,
    labels: Option<AppLabels>,
    #[allow(dead_code)] // Only used to verify we recognize the file
    format: ManifestFormat,
    #[allow(dead_code)] // Only used to verify we recognize the version
    version: ManifestVersion,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ManifestFormat {
    ApolloAiAppManifest,
}

#[derive(Clone, Copy, Deserialize)]
enum ManifestVersion {
    #[serde(rename = "1")]
    V1,
}

#[derive(Clone, Deserialize)]
struct AppLabels {
    #[serde(rename = "toolInvocation/invoking")]
    tool_invocation_invoking: Option<String>,
    #[serde(rename = "toolInvocation/invoked")]
    tool_invocation_invoked: Option<String>,
}

#[derive(Clone, Deserialize)]
struct OperationDefinition {
    /// The GraphQL operation itself
    body: String,
    /// If this operation should be prefetched, this ID indicates where the UI expects to find the data
    #[serde(rename = "prefetchID", default)]
    prefetch_id: Option<String>,
    /// The tools which make up this app
    tools: Vec<ToolDefinition>,
}

#[derive(Clone, Deserialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "extraInputs", default)]
    extra_inputs: Option<Vec<ExtraInputDefinition>>,
    labels: Option<AppLabels>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum JsonSchemaType {
    String,
    Number,
    Boolean,
    Integer,
    Array,
    Object,
}

impl Display for JsonSchemaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            JsonSchemaType::String => "string",
            JsonSchemaType::Number => "number",
            JsonSchemaType::Boolean => "boolean",
            JsonSchemaType::Integer => "integer",
            JsonSchemaType::Array => "array",
            JsonSchemaType::Object => "object",
        })
    }
}

#[derive(Clone, Deserialize)]
struct ExtraInputDefinition {
    name: String,
    description: String,
    #[serde(rename = "type")]
    value_type: JsonSchemaType,
    #[serde(default)]
    required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub(crate) struct CSPSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) connect_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resource_domains: Option<Vec<String>>,
}

#[cfg(test)]
mod test_load_from_path {
    use super::*;
    use assert_fs::{TempDir, prelude::*};

    #[test]
    fn test_local_resource() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MyApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                            "version": "1",
                            "hash": "abcdef",
                            "resource": "index.html",
                            "operations": []}"#,
            )
            .unwrap();
        let html = "<html>blelo</html>";
        app_dir.child("index.html").write_str(html).unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse("type Query { hello: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        match &app.resource {
            AppResource::Local(contents) => assert_eq!(contents, html),
            AppResource::Remote(url) => panic!("unexpected remote resource {url}"),
        }
        assert_eq!(app.uri, "ui://widget/MyApp#abcdef".parse().unwrap());
    }

    #[test]
    fn test_remote_resource() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("RemoteApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                            "version": "1",
                            "hash": "abcdef",
                            "resource": "https://example.com/widget/index.html",
                            "operations": []}"#,
            )
            .unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse("type Query { hello: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        match &app.resource {
            AppResource::Remote(url) => {
                assert_eq!(url.as_str(), "https://example.com/widget/index.html")
            }
            AppResource::Local(contents) => {
                panic!("expected remote resource, found local: {contents}")
            }
        }
    }

    #[test]
    fn multiple_tools_in_an_app() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MultipleToolsApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                    "version": "1",
                    "hash": "abcdef",
                    "resource": "https://example.com/widget/index.html",
                    "operations": [
                        {"body": "query MyOperation { hello }", "tools": [
                          {"name": "Tool1", "description": "Description for Tool1"},
                          {"name": "Tool2", "description": "Description for Tool2"}
                        ]},
                        {"body": "query MyOtherOperation { world }", "tools": [
                          {"name": "Tool3", "description": "Description for Tool3"}
                        ]}
                    ]}"#,
            )
            .unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse(
                "type Query { hello: String, world: String }",
                "schema.graphql",
            )
            .unwrap()
            .validate()
            .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        assert_eq!(app.tools.len(), 3);
        assert_eq!(app.tools[0].tool.name, "MultipleToolsApp--Tool1");
        assert_eq!(
            app.tools[0].tool.description.as_ref().unwrap(),
            "Description for Tool1"
        );
        assert_eq!(
            app.tools[0].operation.inner.source_text,
            "query MyOperation { hello }"
        );
        assert_eq!(app.tools[1].tool.name, "MultipleToolsApp--Tool2");
        assert_eq!(
            app.tools[1].tool.description.as_ref().unwrap(),
            "Description for Tool2"
        );
        assert_eq!(app.tools[2].tool.name, "MultipleToolsApp--Tool3");
        assert_eq!(
            app.tools[2].tool.description.as_ref().unwrap(),
            "Description for Tool3"
        );
    }

    #[test]
    fn prefetch_operations() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MultipleToolsApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                    "version": "1",
                    "hash": "abcdef",
                    "resource": "https://example.com/widget/index.html",
                    "operations": [
                        {"body": "query MyOperation { hello }", "tools": [
                          {"name": "Tool1", "description": "Description for Tool1"}
                        ], "prefetchID": "prefetchId1"},
                        {"body": "query MyOtherOperation { world }", "tools": [], "prefetchID": "prefetchId2"}
                    ]}"#,
            )
            .unwrap();

        let apps = load_from_path(
            temp.path(),
            &Schema::parse(
                "type Query { hello: String, world: String }",
                "schema.graphql",
            )
            .unwrap()
            .validate()
            .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        assert_eq!(app.tools.len(), 1);
        assert_eq!(app.prefetch_operations.len(), 2);
        let prefetch_that_matches_tool = app
            .prefetch_operations
            .iter()
            .find(|prefetch| prefetch.prefetch_id == "prefetchId1")
            .unwrap();
        assert!(
            Arc::ptr_eq(
                &prefetch_that_matches_tool.operation,
                &app.tools[0].operation
            ),
            "Prefetches should be deduplicated via Arc comparison"
        )
    }

    #[test]
    fn should_map_extra_inputs_to_input_schema() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("ExtraInputsApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                    "version": "1",
                    "hash": "abcdef",
                    "resource": "https://example.com/widget/index.html",
                    "operations": [
                        {"body": "query MyOperation { hello }", "tools": [
                          {"name": "Tool1", "description": "Description for Tool1", "extraInputs": [{
                            "name": "isAwesome",
                            "type": "boolean",
                            "description": "Is everything awesome?",
                            "required": true
                          }]}
                        ]}
                    ]}"#,
            )
            .unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse(
                "type Query { hello: String, world: String }",
                "schema.graphql",
            )
            .unwrap()
            .validate()
            .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");

        let input_schema = &apps[0].tools[0].tool.input_schema;

        let properties = input_schema
            .get("properties")
            .expect("Should have a properties property")
            .as_object()
            .expect("Properties should be a map");
        let required = input_schema
            .get("required")
            .expect("Should have a required property")
            .as_array()
            .expect("Required should be an array");

        assert!(properties.contains_key("isAwesome"));
        assert!(required.contains(&Value::String("isAwesome".to_string())));
    }

    #[test]
    fn should_error_when_multiple_extra_inputs_with_same_name() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("ExtraInputsSameNameApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                    "version": "1",
                    "hash": "abcdef",
                    "resource": "https://example.com/widget/index.html",
                    "operations": [
                        {"body": "query MyOperation { hello }", "tools": [
                          {"name": "Tool1", "description": "Description for Tool1", "extraInputs": [{
                            "name": "isAwesome",
                            "type": "boolean",
                            "description": "Is everything awesome?",
                            "required": true
                          },
                          {
                            "name": "isAwesome",
                            "type": "boolean",
                            "description": "Is everything awesome still?",
                            "required": false
                          }
                          ]}
                        ]}
                    ]}"#,
            )
            .unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse(
                "type Query { hello: String, world: String }",
                "schema.graphql",
            )
            .unwrap()
            .validate()
            .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        );

        assert!(apps.is_err());
        assert_eq!(
            apps.err().unwrap(),
            "Extra input with name 'isAwesome' failed to process because another input with this name was already processed. Make sure your extra_input names are unique, both from each other and any graphql variables you may have."
        )
    }

    #[test]
    fn should_error_when_extra_input_name_conflicts_with_graphql_variable() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("ExtraInputsSameNameApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                    "version": "1",
                    "hash": "abcdef",
                    "resource": "https://example.com/widget/index.html",
                    "operations": [
                        {"body": "query MyOperation($isAwesome: Boolean) { hello(isAwesome: $isAwesome) }", "tools": [
                          {"name": "Tool1", "description": "Description for Tool1", "extraInputs": [{
                            "name": "isAwesome",
                            "type": "boolean",
                            "description": "Is everything awesome?",
                            "required": true
                          }
                          ]}
                        ]}
                    ]}"#,
            )
            .unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse(
                "type Query { hello(isAwesome: Boolean): String, world: String }",
                "schema.graphql",
            )
            .unwrap()
            .validate()
            .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        );

        assert!(apps.is_err());
        assert_eq!(
            apps.err().unwrap(),
            "Extra input with name 'isAwesome' failed to process because another input with this name was already processed. Make sure your extra_input names are unique, both from each other and any graphql variables you may have."
        )
    }

    #[test]
    fn should_not_have_tool_invocation_labels_when_not_specified() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MyApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                            "version": "1",
                            "hash": "abcdef",
                            "resource": "index.html",
                            "operations": [
                                {
                                    "body": "query MyOperation { hello }", 
                                    "tools": [
                                        {"name": "Tool1", "description": "Description for Tool1" }
                                    ]
                                }
                            ]}"#,
            )
            .unwrap();
        let html = "<html>blelo</html>";
        app_dir.child("index.html").write_str(html).unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse("type Query { hello: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        let tool = &app.tools[0];

        assert!(
            tool.tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoking")
                .is_none()
        );
        assert!(
            tool.tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoked")
                .is_none()
        );
    }

    #[test]
    fn should_have_tool_invocation_labels_when_specified_in_manifest() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MyApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                            "version": "1",
                            "hash": "abcdef",
                            "resource": "index.html",
                            "operations": [
                                {
                                    "body": "query MyOperation { hello }", 
                                    "tools": [
                                        {"name": "Tool1", "description": "Description for Tool1" },
                                        {"name": "Tool2", "description": "Description for Tool2" }
                                    ]
                                }
                            ],
                            "labels": {
                                "toolInvocation/invoking": "Store is invoking...",
                                "toolInvocation/invoked": "Happy shopping!"
                            }
                        }"#,
            )
            .unwrap();
        let html = "<html>blelo</html>";
        app_dir.child("index.html").write_str(html).unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse("type Query { hello: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        let tool1 = &app.tools[0];
        let tool2 = &app.tools[1];

        assert_eq!(
            tool1
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoking")
                .unwrap(),
            "Store is invoking..."
        );
        assert_eq!(
            tool1
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoked")
                .unwrap(),
            "Happy shopping!"
        );
        assert_eq!(
            tool2
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoking")
                .unwrap(),
            "Store is invoking..."
        );
        assert_eq!(
            tool2
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoked")
                .unwrap(),
            "Happy shopping!"
        );
    }

    #[test]
    fn should_have_tool_invocation_labels_overridden_when_specified_by_tool() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MyApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                            "version": "1",
                            "hash": "abcdef",
                            "resource": "index.html",
                            "operations": [
                                {
                                    "body": "query MyOperation { hello }", 
                                    "tools": [
                                        {"name": "Tool1", "description": "Description for Tool1" },
                                        {"name": "Tool2", "description": "Description for Tool2", "labels": {
                                                "toolInvocation/invoking": "Adding to cart...",
                                                "toolInvocation/invoked": "Cart filled!"
                                            }
                                        }
                                    ]
                                }
                            ],
                            "labels": {
                                "toolInvocation/invoking": "Store is invoking...",
                                "toolInvocation/invoked": "Happy shopping!"
                            }
                        }"#,
            )
            .unwrap();
        let html = "<html>blelo</html>";
        app_dir.child("index.html").write_str(html).unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse("type Query { hello: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        let tool1 = &app.tools[0];
        let tool2 = &app.tools[1];

        assert_eq!(
            tool1
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoking")
                .unwrap(),
            "Store is invoking..."
        );
        assert_eq!(
            tool1
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoked")
                .unwrap(),
            "Happy shopping!"
        );
        assert_eq!(
            tool2
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoking")
                .unwrap(),
            "Adding to cart..."
        );
        assert_eq!(
            tool2
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoked")
                .unwrap(),
            "Cart filled!"
        );
    }

    #[test]
    fn should_have_tool_invocation_labels_when_only_specified_by_tool() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MyApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                            "version": "1",
                            "hash": "abcdef",
                            "resource": "index.html",
                            "operations": [
                                {
                                    "body": "query MyOperation { hello }", 
                                    "tools": [
                                        {"name": "Tool1", "description": "Description for Tool1" },
                                        {"name": "Tool2", "description": "Description for Tool2", "labels": {
                                                "toolInvocation/invoking": "Adding to cart...",
                                                "toolInvocation/invoked": "Cart filled!"
                                            }
                                        }
                                    ]
                                }
                            ]
                        }"#,
            )
            .unwrap();
        let html = "<html>blelo</html>";
        app_dir.child("index.html").write_str(html).unwrap();
        let apps = load_from_path(
            temp.path(),
            &Schema::parse("type Query { hello: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap(),
            None,
            MutationMode::All,
            false,
            false,
            true,
        )
        .expect("Failed to load apps");
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        let tool1 = &app.tools[0];
        let tool2 = &app.tools[1];

        assert!(
            tool1
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoking")
                .is_none()
        );
        assert!(
            tool1
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoked")
                .is_none(),
        );
        assert_eq!(
            tool2
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoking")
                .unwrap(),
            "Adding to cart..."
        );
        assert_eq!(
            tool2
                .tool
                .meta
                .clone()
                .unwrap()
                .get("openai/toolInvocation/invoked")
                .unwrap(),
            "Cart filled!"
        );
    }
}
