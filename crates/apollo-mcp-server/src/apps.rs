use std::path::Path;
use std::{fs::read_to_string, sync::Arc};

use apollo_compiler::{Schema, validation::Valid};
use rmcp::model::{Meta, RawResource, Resource, Tool};
use serde::Deserialize;
use tracing::debug;
use url::Url;

use crate::{
    custom_scalar_map::CustomScalarMap,
    operations::{MutationMode, Operation, RawOperation},
};

/// An app, which consists of a tool and a resource to be used together.
#[derive(Clone, Debug)]
pub(crate) struct App {
    pub(crate) name: String,
    /// The HTML resource that serves as the app's UI
    pub(crate) resource: AppResource,
    /// The URI of the app's resource
    pub(crate) uri: Url,
    /// Entrypoint tools for this app
    pub(crate) tools: Vec<AppTool>,
}

/// An MCP tool which serves as an entrypoint for an app.
#[derive(Clone, Debug)]
pub(crate) struct AppTool {
    /// The GraphQL operation that's executed when the tool is called. Its data is injected into the UI
    pub(crate) operation: Arc<Operation>,
    /// The MCP tool definition
    pub(crate) tool: Tool,
}

#[derive(Clone, Debug)]
pub(crate) enum AppResource {
    Local(String),
    Remote(Url),
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
                    input_schema: operation.tool.input_schema.clone(),
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
            tools,
        });
    }
    Ok(apps)
}

#[derive(Clone, Deserialize)]
struct Manifest {
    hash: String,
    operations: Vec<OperationDefinition>,
    resource: String,
    name: Option<String>,
    description: Option<String>,
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
struct OperationDefinition {
    /// The GraphQL operation itself
    body: String,
    /// If this operation should be prefetched, this ID indicates where the UI expects to find the data
    #[serde(rename = "prefetchID", default)]
    #[allow(dead_code)] // Will use in follow-up PR
    prefetch_id: Option<String>,
    /// The tools which make up this app
    tools: Vec<ToolDefinition>,
}

#[derive(Clone, Deserialize)]
struct ToolDefinition {
    name: String,
    description: String,
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
}
