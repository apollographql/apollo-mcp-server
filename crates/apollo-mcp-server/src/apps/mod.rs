use std::{collections::BTreeMap, sync::Arc};

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

/// Manifest tool names to expose on connections without an app parameter, by app name.
pub type AppAllowlist = BTreeMap<String, Vec<String>>;

/// Exposed tool name to owning app name. Tool names are unique across the selected apps.
pub(crate) type AppExports = Arc<BTreeMap<String, String>>;

fn sole<T>(
    mut matches: impl Iterator<Item = T>,
    what: impl std::fmt::Display,
) -> Result<T, String> {
    match (matches.next(), matches.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(format!("unknown {what}")),
        _ => Err(format!("ambiguous {what}")),
    }
}

pub(crate) fn resolve_exports(
    apps: &[App],
    allowlist: &AppAllowlist,
    is_builtin: impl Fn(&str) -> bool,
) -> Result<AppExports, String> {
    let mut exports = BTreeMap::new();
    let mut paths = BTreeMap::new();
    for (app_name, tools) in allowlist {
        let app = sole(
            apps.iter().filter(|app| app.name == *app_name),
            format_args!("app {app_name}"),
        )?;
        // Shared resource reads identify the app by its URI path alone.
        if let Some(other) = paths.insert(app.uri.path(), app_name) {
            return Err(format!(
                "{other} and {app_name} share resource path {}",
                app.uri.path()
            ));
        }
        for tool in tools {
            sole(
                app.tools.iter().filter(|t| t.tool.name == *tool),
                format_args!("tool {app_name}/{tool}"),
            )?;
            if is_builtin(tool) {
                return Err(format!(
                    "{app_name}/{tool} conflicts with an enabled built-in tool"
                ));
            }
            if let Some(other) = exports.insert(tool.clone(), app_name.clone()) {
                return Err(format!("{tool} claimed by {other} and {app_name}"));
            }
        }
    }
    Ok(Arc::new(exports))
}

pub(crate) fn warn_shadowed(exports: &AppExports, operations: &[crate::operations::Operation]) {
    for operation in operations {
        let name: &str = &operation.as_ref().name;
        if let Some(app) = exports.get(name) {
            tracing::warn!(%name, %app, "Exposed app tool shadows an operation of the same name");
        }
    }
}

/// An exposed app tool never inherits the tokenless exception of an operation it shadows.
pub(crate) fn drop_exported_skips(tools: &mut Vec<String>, exports: &AppExports) {
    tools.retain(|tool| {
        let exported = exports.contains_key(tool);
        if exported {
            tracing::warn!(%tool, "skip_token_validation.tools entry ignored for exposed app tool");
        }
        !exported
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_only_exported_names_from_the_skip_list() {
        let exports = Arc::new([("lookup".to_owned(), "records".to_owned())].into());
        let mut tools = vec!["lookup".to_owned(), "Public".to_owned()];
        drop_exported_skips(&mut tools, &exports);
        assert_eq!(tools, ["Public"]);
    }
}
