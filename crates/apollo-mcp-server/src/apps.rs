use std::fs::read_to_string;
use std::path::Path;

use apollo_compiler::{Schema, validation::Valid};
use rmcp::model::{Meta, RawResource, Resource};
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
    /// The GraphQL operation that defines the app's tool
    pub(crate) operation: Operation,
    /// The HTML resource that serves as the app's UI
    pub(crate) resource: AppResource,
    /// The URI of the app's resource
    pub(crate) uri: Url,
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
                name: self.operation.tool.name.clone().into(),
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

        let Some(operation_str) = manifest
            .operations
            .iter()
            .find_map(|op| op.prefetch_id.is_some().then(|| op.body.to_string()))
        else {
            // TODO: Allow applications with only post-fetch operations
            return Err("Exactly one prefetch operation must be defined".into());
        };

        let raw = RawOperation::from((operation_str, path.to_str().map(String::from)));
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
                    "No operation in {path}",
                    path = path.to_string_lossy()
                ));
            }
            Ok(Some(op)) => op,
        };

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
        operation.tool.name = name.into();
        operation.tool.meta = Some(meta);
        if let Some(description) = manifest.description {
            operation.tool.description = Some(description.into());
        }

        apps.push(App {
            uri,
            operation,
            resource,
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
        assert_eq!(app.operation.tool.name, "MyApp");
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
    }
}
