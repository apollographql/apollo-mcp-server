use std::sync::Arc;

use rmcp::model::{ClientCapabilities, ErrorCode, Extensions, RawResource, Resource, Tool};
use serde_json::Value;
use url::Url;

use crate::apps::manifest::{AppLabels, CSPSettings, WidgetSettings};
use crate::errors::McpError;
use crate::operations::Operation;

/// An app, which consists of a tool and a resource to be used together.
#[derive(Clone, Debug)]
pub(crate) struct App {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// The HTML resource that serves as the app's UI
    pub(crate) resource: AppResource,
    /// Any CSP settings to apply to the resource
    pub(crate) csp_settings: Option<CSPSettings>,
    /// Various resource meta data
    pub(crate) widget_settings: Option<WidgetSettings>,
    /// The URI of the app's resource
    pub(crate) uri: Url,
    /// Entrypoint tools for this app
    pub(crate) tools: Vec<AppTool>,
    /// Any operations that should _always_ be executed for any of the tools (after the initial tool operation)
    pub(crate) prefetch_operations: Vec<PrefetchOperation>,
}

#[derive(Clone, Debug)]
pub(crate) enum AppResource {
    Targeted(TargetedAppResource),
    Single(AppResourceSource),
}

#[derive(Clone, Debug)]
pub(crate) struct TargetedAppResource {
    pub(crate) openai: Option<AppResourceSource>,
    pub(crate) mcp: Option<AppResourceSource>,
}

#[derive(Clone, Debug)]
pub(crate) enum AppResourceSource {
    Local(String),
    Remote(Url),
}

/// An MCP tool which serves as an entrypoint for an app.
#[derive(Clone, Debug)]
pub(crate) struct AppTool {
    /// The GraphQL operation that's executed when the tool is called. Its data is injected into the UI
    pub(crate) operation: Arc<Operation>,
    // The labels for this tool
    pub(crate) labels: AppLabels,
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
                mime_type: None,
                // TODO: load all this from a manifest file
                title: None,
                description: self.description.clone(),
                icons: None,
                size: None,
                meta: None,
            },
            None,
        )
    }
}

pub(crate) enum AppTarget {
    AppsSDK,
    MCPApps,
}

impl TryFrom<(Extensions, Option<&ClientCapabilities>)> for AppTarget {
    type Error = McpError;

    fn try_from(
        (extensions, client_capabilities): (Extensions, Option<&ClientCapabilities>),
    ) -> Result<Self, Self::Error> {
        let app_target_param = extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.uri.query())
            .and_then(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .find(|(key, _)| key == "appTarget")
                    .map(|(_, value)| value.into_owned())
            });

        match app_target_param {
            Some(app_target) if app_target.to_lowercase() == "openai" => Ok(AppTarget::AppsSDK),
            Some(app_target) if app_target.to_lowercase() == "mcp" => Ok(AppTarget::MCPApps),
            Some(app_target) => Err(McpError::new(
                ErrorCode::INVALID_REQUEST,
                format!(
                    "App target {app_target} not recognized. Valid values are 'openai' or 'mcp'."
                ),
                None,
            )),
            None => {
                // If target hasn't been specified in the URL, try to detect it via client capabilities. If we still don't know, we'll default to AppsSDK.
                if let Some(client_capabilities) = client_capabilities
                    && has_mcp_app_support(client_capabilities)
                {
                    Ok(AppTarget::MCPApps)
                } else {
                    Ok(AppTarget::AppsSDK)
                }
            }
        }
    }
}

pub(crate) fn has_mcp_app_support(client_capabilities: &ClientCapabilities) -> bool {
    client_capabilities
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("io.modelcontextprotocol/ui"))
        .and_then(|extension| extension.get("mimeTypes"))
        .and_then(|mimetypes| mimetypes.as_array())
        .is_some_and(|mimetypes| mimetypes.contains(&Value::from("text/html;profile=mcp-app")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_target_openai_lowercase() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=openai")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let app_target = AppTarget::try_from((extensions, None)).unwrap();
        assert!(matches!(app_target, AppTarget::AppsSDK));
    }

    #[test]
    fn test_app_target_openai_uppercase() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=OPENAI")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let app_target = AppTarget::try_from((extensions, None)).unwrap();
        assert!(matches!(app_target, AppTarget::AppsSDK));
    }

    #[test]
    fn test_app_target_mcp_lowercase() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=mcp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let app_target = AppTarget::try_from((extensions, None)).unwrap();
        assert!(matches!(app_target, AppTarget::MCPApps));
    }

    #[test]
    fn test_app_target_mcp_uppercase() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=MCP")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let app_target = AppTarget::try_from((extensions, None)).unwrap();
        assert!(matches!(app_target, AppTarget::MCPApps));
    }

    #[test]
    fn test_app_target_invalid_value() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=invalid")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = AppTarget::try_from((extensions, None));
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err.code, ErrorCode::INVALID_REQUEST);
        assert!(
            err.message
                .contains("App target invalid not recognized. Valid values are 'openai' or 'mcp'.")
        );
    }

    #[test]
    fn test_app_target_missing_defaults_to_apps_sdk() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let app_target = AppTarget::try_from((extensions, None)).unwrap();
        assert!(matches!(app_target, AppTarget::AppsSDK));
    }

    #[test]
    fn test_app_target_missing_with_mcp_app_capability_defaults_to_mcp_apps() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let mut extension_capabilities = std::collections::BTreeMap::new();
        extension_capabilities.insert(
            "io.modelcontextprotocol/ui".to_string(),
            serde_json::json!({"mimeTypes": ["text/html;profile=mcp-app"]})
                .as_object()
                .unwrap()
                .clone(),
        );
        let client_capabilities = ClientCapabilities {
            extensions: Some(extension_capabilities),
            ..Default::default()
        };

        let app_target = AppTarget::try_from((extensions, Some(&client_capabilities))).unwrap();
        assert!(matches!(app_target, AppTarget::MCPApps));
    }
}
