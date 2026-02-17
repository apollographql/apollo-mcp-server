use rmcp::ErrorData;
use rmcp::model::{Meta, Resource, ResourceContents};
use serde_json::json;
use url::Url;

use crate::apps::app::{AppResource, AppResourceSource, AppTarget};

use super::App;

const MCP_MIME_TYPE: &str = "text/html;profile=mcp-app";

pub(crate) fn attach_resource_mime_type(mut resource: Resource) -> Resource {
    resource.raw.mime_type = Some(MCP_MIME_TYPE.to_string());
    resource
}

pub(crate) async fn get_app_resource(
    apps: &[App],
    request: rmcp::model::ReadResourceRequestParams,
    request_uri: Url,
    app_target: &AppTarget,
) -> Result<ResourceContents, ErrorData> {
    let Some(app) = apps.iter().find(|app| app.uri.path() == request_uri.path()) else {
        return Err(ErrorData::resource_not_found(
            format!("Resource not found for URI: {}", request.uri),
            None,
        ));
    };

    let resource_source = match &app.resource {
        AppResource::Targeted(resource) => match app_target {
            AppTarget::AppsSDK => resource.openai.as_ref().ok_or_else(|| {
                ErrorData::resource_not_found(
                    "Invalid apps target: no resource found for openai".to_string(),
                    None,
                )
            })?,
            AppTarget::MCPApps => resource.mcp.as_ref().ok_or_else(|| {
                ErrorData::resource_not_found(
                    "Invalid apps target: no resource found for mcp".to_string(),
                    None,
                )
            })?,
        },
        AppResource::Single(app_resource_source) => app_resource_source,
    };

    let text = match resource_source {
        AppResourceSource::Local(contents) => contents.clone(),
        AppResourceSource::Remote(url) => {
            let response = reqwest::Client::new()
                .get(url.clone())
                .send()
                .await
                .map_err(|err| {
                    ErrorData::resource_not_found(
                        format!("Failed to fetch resource from {}: {err}", url),
                        None,
                    )
                })?;

            if !response.status().is_success() {
                return Err(ErrorData::resource_not_found(
                    format!(
                        "Failed to fetch resource from {}: received status {}",
                        url,
                        response.status()
                    ),
                    None,
                ));
            }

            response.text().await.map_err(|err| {
                ErrorData::resource_not_found(
                    format!("Failed to read resource body from {}: {err}", url),
                    None,
                )
            })?
        }
    };

    // Most properties now are listed under _meta.ui.* but some openai specific properties are still at the root
    // So, we will populate both and then nest "ui" into "meta" later in this function
    let mut meta: Option<Meta> = None;
    let mut ui: Option<Meta> = None;
    if let Some(csp) = &app.csp_settings {
        ui.get_or_insert_with(Meta::new).insert(
            "csp".into(),
            json!({
                "connectDomains": csp.connect_domains,
                "resourceDomains": csp.resource_domains,
                "frameDomains": csp.frame_domains,
                "baseUriDomains": csp.base_uri_domains
            }),
        );

        // Openai has a weird bug where it won't merge these settings with the MCP ones... so we just have to set both.
        if matches!(app_target, AppTarget::AppsSDK) {
            meta.get_or_insert_with(Meta::new).insert(
                "openai/widgetCSP".into(),
                json!({
                    "connect_domains": csp.connect_domains,
                    "resource_domains": csp.resource_domains,
                    "frame_domains": csp.frame_domains,
                    "redirect_domains": csp.redirect_domains
                }),
            );
        }
    }

    if let Some(widget_settings) = &app.widget_settings {
        if let Some(description) = &widget_settings.description
            && matches!(app_target, AppTarget::AppsSDK)
        {
            meta.get_or_insert_with(Meta::new).insert(
                "openai/widgetDescription".into(),
                serde_json::to_value(description).unwrap_or_default(),
            );
        }

        if let Some(domain) = &widget_settings.domain {
            ui.get_or_insert_with(Meta::new).insert(
                "domain".into(),
                serde_json::to_value(domain).unwrap_or_default(),
            );
        }

        if let Some(prefers_border) = &widget_settings.prefers_border {
            ui.get_or_insert_with(Meta::new).insert(
                "prefersBorder".into(),
                serde_json::to_value(prefers_border).unwrap_or_default(),
            );
        }
    }

    meta.get_or_insert_with(Meta::new)
        .insert("ui".into(), serde_json::to_value(ui).unwrap_or_default());

    Ok(ResourceContents::TextResourceContents {
        uri: request.uri,
        mime_type: Some(MCP_MIME_TYPE.to_string()),
        text,
        meta,
    })
}

#[cfg(test)]
mod tests {
    use rmcp::model::{Extensions, RawResource};

    use crate::apps::app::TargetedAppResource;
    use crate::apps::manifest::{CSPSettings, WidgetSettings};

    use super::*;

    #[test]
    fn attach_correct_mime_type() {
        let resource = Resource::new(
            RawResource {
                name: "TestResource".to_string(),
                uri: "ui://test".to_string(),
                mime_type: None,
                title: None,
                description: None,
                icons: None,
                size: None,
                meta: None,
            },
            None,
        );

        let result = attach_resource_mime_type(resource);

        assert_eq!(
            result.raw.mime_type,
            Some("text/html;profile=mcp-app".to_string())
        );
    }

    #[test]
    fn errors_when_invalid_target_provided() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=lol")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);
        let app_target = AppTarget::try_from(extensions);

        assert!(app_target.is_err());
        assert_eq!(
            app_target.err().unwrap().message,
            "App target lol not recognized. Valid values are 'openai' or 'mcp'."
        )
    }

    #[tokio::test]
    async fn get_app_resource_returns_openai_format_when_target_is_openai() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test content".to_string())),
            csp_settings: Some(CSPSettings {
                connect_domains: Some(vec!["connect.example.com".to_string()]),
                resource_domains: Some(vec!["resource.example.com".to_string()]),
                frame_domains: Some(vec!["frame.example.com".to_string()]),
                redirect_domains: Some(vec!["redirect.example.com".to_string()]),
                base_uri_domains: Some(vec!["base.example.com".to_string()]),
            }),
            widget_settings: Some(WidgetSettings {
                description: Some("Test description".to_string()),
                domain: Some("example.com".to_string()),
                prefers_border: Some(true),
            }),
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParams {
                uri: "ui://widget/TestApp".to_string(),
                meta: None,
            },
            "ui://widget/TestApp".parse().unwrap(),
            &AppTarget::AppsSDK,
        )
        .await
        .unwrap();

        let ResourceContents::TextResourceContents {
            mime_type, meta, ..
        } = result
        else {
            unreachable!()
        };
        assert_eq!(mime_type, Some("text/html;profile=mcp-app".to_string()));

        let meta = meta.unwrap();
        let csp = meta.get("openai/widgetCSP").unwrap();
        assert!(csp.get("connect_domains").is_some());
        assert!(csp.get("resource_domains").is_some());
        assert!(csp.get("frame_domains").is_some());
        assert!(csp.get("redirect_domains").is_some());
        // OpenAI-specific description should be at root
        assert!(meta.get("openai/widgetDescription").is_some());
        // ui nesting should contain the common properties
        let ui_meta = meta.get("ui").unwrap();
        let ui_csp = ui_meta.get("csp").unwrap();
        assert!(ui_csp.get("connectDomains").is_some());
        assert!(ui_csp.get("resourceDomains").is_some());
        assert!(ui_csp.get("frameDomains").is_some());
        assert!(ui_csp.get("baseUriDomains").is_some());
        assert!(ui_meta.get("domain").is_some());
        assert!(ui_meta.get("prefersBorder").is_some());
    }

    #[tokio::test]
    async fn get_app_resource_returns_mcp_format_when_target_is_mcp() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test content".to_string())),
            csp_settings: Some(CSPSettings {
                connect_domains: Some(vec!["connect.example.com".to_string()]),
                resource_domains: Some(vec!["resource.example.com".to_string()]),
                frame_domains: Some(vec!["frame.example.com".to_string()]),
                redirect_domains: Some(vec!["redirect.example.com".to_string()]),
                base_uri_domains: Some(vec!["base.example.com".to_string()]),
            }),
            widget_settings: Some(WidgetSettings {
                description: Some("Test description".to_string()),
                domain: Some("example.com".to_string()),
                prefers_border: Some(true),
            }),
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParams {
                uri: "ui://widget/TestApp".to_string(),
                meta: None,
            },
            "ui://widget/TestApp".parse().unwrap(),
            &AppTarget::MCPApps,
        )
        .await
        .unwrap();

        let ResourceContents::TextResourceContents {
            mime_type, meta, ..
        } = result
        else {
            unreachable!()
        };
        assert_eq!(mime_type, Some("text/html;profile=mcp-app".to_string()));

        let meta = meta.unwrap();
        // MCPApps should have ui nesting
        let ui_meta = meta.get("ui").unwrap();
        // MCPApps CSP uses camelCase keys and includes baseUriDomains (not redirectDomains)
        let csp = ui_meta.get("csp").unwrap();
        assert!(csp.get("connectDomains").is_some());
        assert!(csp.get("resourceDomains").is_some());
        assert!(csp.get("frameDomains").is_some());
        assert!(csp.get("baseUriDomains").is_some());
        assert!(csp.get("redirectDomains").is_none());
        assert!(ui_meta.get("domain").is_some());
        assert!(ui_meta.get("prefersBorder").is_some());
        // MCPApps should not have description
        assert!(ui_meta.get("description").is_none());
    }

    #[tokio::test]
    async fn get_app_resource_returns_error_for_nonexistent_resource() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test content".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParams {
                uri: "ui://widget/NonExistent".to_string(),
                meta: None,
            },
            "ui://widget/NonExistent".parse().unwrap(),
            &AppTarget::AppsSDK,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn should_return_openai_content_when_targeted_resource_and_target_is_openai() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Targeted(TargetedAppResource {
                openai: Some(AppResourceSource::Local("openai content".to_string())),
                mcp: Some(AppResourceSource::Local("mcp content".to_string())),
            }),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParams {
                uri: "ui://widget/TestApp".to_string(),
                meta: None,
            },
            "ui://widget/TestApp".parse().unwrap(),
            &AppTarget::AppsSDK,
        )
        .await
        .unwrap();

        let ResourceContents::TextResourceContents { text, .. } = result else {
            unreachable!()
        };
        assert_eq!(text, "openai content");
    }

    #[tokio::test]
    async fn should_return_mcp_content_when_targeted_resource_and_target_is_mcp() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Targeted(TargetedAppResource {
                openai: Some(AppResourceSource::Local("openai content".to_string())),
                mcp: Some(AppResourceSource::Local("mcp content".to_string())),
            }),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParams {
                uri: "ui://widget/TestApp".to_string(),
                meta: None,
            },
            "ui://widget/TestApp".parse().unwrap(),
            &AppTarget::MCPApps,
        )
        .await
        .unwrap();

        let ResourceContents::TextResourceContents { text, .. } = result else {
            unreachable!()
        };
        assert_eq!(text, "mcp content");
    }

    #[tokio::test]
    async fn should_return_error_when_targeted_resource_missing_for_requested_target() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Targeted(TargetedAppResource {
                openai: Some(AppResourceSource::Local("openai content".to_string())),
                mcp: None,
            }),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParams {
                uri: "ui://widget/TestApp".to_string(),
                meta: None,
            },
            "ui://widget/TestApp".parse().unwrap(),
            &AppTarget::MCPApps,
        )
        .await;

        assert!(result.is_err());
    }
}
