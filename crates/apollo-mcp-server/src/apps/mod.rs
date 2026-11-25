pub(crate) mod app;
pub(crate) mod manifest;
pub(crate) mod resource;
pub(crate) mod tool;

pub(crate) use app::App;
pub(crate) use manifest::load_from_path;

/// The query parameter that scopes a request to a single app.
pub(crate) const APP_QUERY_PARAM: &str = "app";

/// Reads the app name out of a request's query string.
///
/// The auth middleware and the tool dispatcher both decide what a request
/// targets, so they read this parameter through one function to keep those two
/// answers from drifting apart.
pub(crate) fn app_param_from_query(query: Option<&str>) -> Option<String> {
    query.and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == APP_QUERY_PARAM)
            .map(|(_, value)| value.into_owned())
    })
}

pub type AppAllowlist = std::collections::BTreeMap<String, Vec<String>>;

/// Resolved against the immutable app manifests at startup. Runtime operation refreshes
/// only replace ordinary operations, so these indices stay valid for the server lifetime.
#[derive(Clone, Debug, Default)]
pub(crate) struct AppExports(std::sync::Arc<std::collections::BTreeMap<String, Export>>);

#[derive(Debug)]
pub(crate) struct Export {
    pub app_name: String,
    app: usize,
    tool: usize,
}

impl Export {
    pub(crate) fn target<'a>(
        &self,
        apps: &'a [App],
    ) -> Result<(&'a App, &'a app::AppTool), crate::errors::McpError> {
        apps.get(self.app)
            .and_then(|app| app.tools.get(self.tool).map(|tool| (app, tool)))
            .ok_or_else(|| {
                crate::errors::McpError::internal_error(
                    "Resolved app export missing from loaded manifests",
                    None,
                )
            })
    }
}

impl AppExports {
    pub(crate) fn resolve(apps: &[App], allowlist: &AppAllowlist) -> Result<Self, String> {
        let mut exports = std::collections::BTreeMap::new();
        for (name, tools) in allowlist {
            let mut matches = apps.iter().enumerate().filter(|(_, app)| &app.name == name);
            let Some((index, app)) = matches.next() else {
                return Err(format!("apps.expose_on_connect: unknown app {name}"));
            };
            if matches.next().is_some() {
                return Err(format!("apps.expose_on_connect: ambiguous app {name}"));
            }
            for tool_name in tools {
                let mut matches = app
                    .tools
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.tool.name == *tool_name);
                let Some((tool, _)) = matches.next() else {
                    return Err(format!(
                        "apps.expose_on_connect: unknown tool {name}/{tool_name}"
                    ));
                };
                if matches.next().is_some() {
                    return Err(format!(
                        "apps.expose_on_connect: ambiguous tool {name}/{tool_name}"
                    ));
                }
                if !rmcp::handler::server::tool_name_validation::validate_and_warn_tool_name(
                    tool_name,
                ) {
                    return Err(format!(
                        "apps.expose_on_connect: invalid MCP tool name {tool_name} in {name}"
                    ));
                }
                let export = Export {
                    app: index,
                    app_name: name.clone(),
                    tool,
                };
                if let Some(previous) = exports.insert(tool_name.clone(), export) {
                    return Err(format!(
                        "apps.expose_on_connect: {tool_name} claimed by {} and {name}",
                        previous.app_name
                    ));
                }
            }
        }
        Ok(Self(exports.into()))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Export> {
        self.0.get(name)
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &Export)> {
        self.0.iter()
    }
    pub(crate) fn includes(&self, app: usize) -> bool {
        self.0.values().any(|e| e.app == app)
    }

    pub(crate) fn warn_shadowed(&self, operations: &[crate::operations::Operation]) {
        for operation in operations {
            let name = &operation.as_ref().name;
            if let Some(export) = self.get(name) {
                tracing::warn!(%name, app = %export.app_name, "Configured app export shadows ordinary operation");
            }
        }
    }
}
