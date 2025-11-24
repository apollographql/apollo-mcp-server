use std::path::Path;
use std::{fs::read_to_string, sync::Arc};

use apollo_compiler::{Schema, validation::Valid};
use rmcp::model::{Meta, RawResource, Resource, Tool};
use serde::Deserialize;
use serde_json::{Map, Value, json};
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
    /// Prefetch identifier from the manifest, used to nest tool responses
    //pub(crate) prefetch_id: String,
    pub(crate) tools: Vec<AppTool>,
}

#[derive(Clone, Debug)]
pub(crate) struct AppTool {
    pub(crate) operation: Operation,
    pub(crate) prefetch_operations: Vec<(String, Operation)>, //pub(crate) prefetch_id: String,
    pub(crate) extra_inputs: Option<Value>,
}

fn merge_inputs(orig: &mut Map<String, Value>, extra: &Map<String, Value>) {
    // Add properties
    if let Some(Value::Object(extra_props)) = extra.get("properties") {
        let props = orig
            .entry("properties")
            .or_insert_with(|| Value::Object(Map::new()));

        if let Value::Object(orig_props) = props {
            for (k, v) in extra_props {
                orig_props.insert(k.clone(), v.clone());
            }
        }
    }

    // Add required
    if let Some(Value::Array(extra_req)) = extra.get("required") {
        let req = orig
            .entry("required")
            .or_insert_with(|| Value::Array(vec![]));

        if let Value::Array(orig_req) = req {
            for v in extra_req {
                if !orig_req.contains(v) {
                    orig_req.push(v.clone());
                }
            }
        }
    }
}

impl AppTool {
    pub fn as_tool(&self) -> Tool {
        if let Some(extra_inputs) = &self.extra_inputs
            && let Some(extra_inputs) = extra_inputs.as_object()
        {
            let mut base_tool = self.operation.as_ref().clone();
            let original_input_schema = base_tool.input_schema;

            let mut merged = original_input_schema.as_ref().clone();
            merge_inputs(&mut merged, extra_inputs);
            base_tool.input_schema = Arc::new(merged);

            base_tool
        } else {
            self.operation.as_ref().clone()
        }
    }
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
        let Ok(app_dir) = app_dir else {
            // Could not read the directory, potentially a permissions issue
            continue;
        };
        let path = app_dir.path();
        if !path.is_dir() {
            // Don't care about standalone files in this directory
            continue;
        }

        let Ok(manifest) = read_to_string(path.join(MANIFEST_FILE_NAME)) else {
            // A directory without a manifest file is not an application, might be some other build artifact
            continue;
        };

        let manifest: Manifest = serde_json::from_str(&manifest).map_err(|err| {
            format!(
                "Failed to parse manifest from {}: {}",
                path.to_string_lossy(),
                err
            )
        })?;

        let prefetch_operations = manifest
            .operations
            .clone()
            .into_iter()
            .filter(|operation| operation.prefetch)
            .map(|operation_def| {
                let Some(prefetch_id) = operation_def.prefetch_id else {
                    return Err(format!(
                        "Failed to parse operation from {path}: {err}",
                        path = path.to_string_lossy(),
                        err = "Operation marked as prefetch but no prefetchID was provided"
                    ));
                };

                println!("About to parse: {:?}", operation_def.body);
                let raw = RawOperation::from((operation_def.body, path.to_str().map(String::from)));
                match Operation::from_document(
                    raw,
                    schema,
                    custom_scalar_map,
                    mutation_mode,
                    disable_type_description,
                    disable_schema_description,
                ) {
                    Err(err) => Err(format!(
                        "Failed to parse operation from {path}: {err}",
                        path = path.to_string_lossy()
                    )),
                    Ok(None) => Err(format!(
                        "Failed creating prefetch operation: No operation in {path}",
                        path = path.to_string_lossy()
                    )),
                    Ok(Some(op)) => Ok((operation_def.id, prefetch_id, op)),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;

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

        let name = manifest
            .name
            .unwrap_or_else(|| app_dir.file_name().to_string_lossy().to_string());
        let uri_string = format!("ui://widget/{name}#{}", manifest.hash);
        let uri = Url::parse(&uri_string)
            .map_err(|err| format!("Failed to create a URI for resource {uri_string}: {err}",))?;

        let mut meta = Meta::new();
        meta.insert("openai/outputTemplate".to_string(), uri.to_string().into());
        meta.insert("openai/widgetAccessible".to_string(), true.into());

        // TODO: Def should be able to write this a lot better and optimize to not re-create stuff on every loop iteration... and so many clones
        let tools = manifest
            .operations
            .iter()
            .flat_map(|operation_def| {
                operation_def.tools.iter().map(|tool| {
                    let raw = RawOperation::from((
                        operation_def.body.clone(),
                        path.to_str().map(String::from),
                    ));
                    let mut operation = match Operation::from_document(
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
                        Ok(Some(op)) => op,
                    };

                    let tool_name: String = format!("{}--{}", name.clone(), tool.name);

                    operation.tool.name = tool_name.clone().into();
                    operation.tool.meta = Some(meta.clone());
                    if let Some(description) = manifest.description.clone() {
                        operation.tool.description =
                            Some(format!("{} {}", description, tool.description).into());
                    }

                    let extra_inputs = if let Some(extra_inputs) = &tool.extra_inputs {
                        let mut properties = Map::<String, Value>::new();
                        let mut required = Vec::new();
                        extra_inputs.iter().for_each(|extra_input| {
                            if extra_input.required {
                                required.push(extra_input.name.clone());
                            }

                            properties.insert(
                                extra_input.name.clone(),
                                json!({
                                    "description": extra_input.description,
                                    "type": extra_input.value_type
                                }),
                            );
                        });

                        Some(json!({"type": "object", "properties": properties, "required": required}))
                    } else {
                        None
                    };

                    // Collect any prefetch operations that are NOT the same operation we will already run for this tool
                    // If we didn't do this, we would run an operation twice if it was both @prefetch and @tool
                    let prefetch_operations = prefetch_operations
                        .iter()
                        .filter(|(id, ..)| id != &operation_def.id)
                        .map(|(_, prefetch_id, op)| (prefetch_id.clone(), op.clone()))
                        .collect::<Vec<(String, Operation)>>();

                    Ok(AppTool {
                        operation,
                        prefetch_operations,
                        extra_inputs
                    })
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

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
    body: String,
    #[serde(rename = "prefetchID", default)]
    prefetch_id: Option<String>,
    tools: Vec<ToolDefinition>,
    prefetch: bool,
    id: String,
}

#[derive(Clone, Deserialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "extraInputs", default)]
    extra_inputs: Option<Vec<ExtraInputDefinition>>,
}

#[derive(Clone, Deserialize)]
struct ExtraInputDefinition {
    name: String,
    description: String,
    #[serde(rename = "type")]
    value_type: String,
    #[serde(default)]
    required: bool,
}

#[cfg(test)]
mod test_load_from_path {
    use super::*;
    use assert_fs::{TempDir, prelude::*};

    #[test]
    fn test_happy_path() {
        let temp = TempDir::new().expect("Could not create temporary directory for test");
        let app_dir = temp.child("MyApp");
        app_dir
            .child(MANIFEST_FILE_NAME)
            .write_str(
                r#"{"format": "apollo-ai-app-manifest",
                            "version": "1",
                            "hash": "abcdef",
                            "resource": "index.html",
                            "operations": [{"body": "query MyOperation { hello }", "prefetch": true, "prefetchID": "__anonymous"}]}"#,
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
        assert_eq!(app.tools[0].operation.tool.name, "MyApp");
        //assert_eq!(app.prefetch_id, "__anonymous");
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
                            "operations": [{"body": "query MyOperation { hello }", "prefetch": true, "prefetchID": "__anonymous"}]}"#,
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
        //assert_eq!(app.prefetch_id, "__anonymous");
    }
}
