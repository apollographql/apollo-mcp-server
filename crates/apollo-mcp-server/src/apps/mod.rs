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

/// Exposed tool name to owning app name. Tool names are unique across the apps.
pub(crate) type AppExports = Arc<BTreeMap<String, String>>;

/// Maps every app tool to its app, for connections without an app parameter.
///
/// Fails when the apps can't share one connection: two apps have the same name or
/// resource path, two tools have the same name, or a tool has the name of an enabled
/// built-in tool.
pub(crate) fn resolve_exports(
    apps: &[App],
    is_builtin: impl Fn(&str) -> bool,
) -> Result<AppExports, String> {
    let mut exports = BTreeMap::new();
    let mut paths = BTreeMap::new();
    for app in apps {
        let name = &app.name;
        let path = app.uri.path();
        // Shared resource reads identify the app by its URI path alone.
        if let Some(other) = paths.insert(path, name) {
            return Err(if other == name {
                format!("two apps are named {name}")
            } else {
                format!("{other} and {name} share resource path {path}")
            });
        }
        for tool in &app.tools {
            let tool = tool.tool.name.as_ref();
            if is_builtin(tool) {
                return Err(format!(
                    "{name}/{tool} conflicts with an enabled built-in tool"
                ));
            }
            if let Some(other) = exports.insert(tool.to_owned(), name.clone()) {
                return Err(if other == *name {
                    format!("{name} has two tools named {tool}")
                } else {
                    format!("{other} and {name} both have a tool named {tool}")
                });
            }
        }
    }
    Ok(Arc::new(exports))
}

pub(crate) fn warn_shadowed(exports: &AppExports, operations: &[crate::operations::Operation]) {
    operations
        .iter()
        .filter_map(|operation| exports.get_key_value(&*operation.as_ref().name))
        .for_each(|(name, app)| {
            tracing::warn!(%name, %app, "Exposed app tool shadows an operation of the same name")
        });
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
