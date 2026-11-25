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

pub(crate) fn resolve_exports(
    apps: &[App],
    allowlist: &AppAllowlist,
    is_builtin: impl Fn(&str) -> bool,
) -> Result<AppExports, String> {
    let mut exports = BTreeMap::new();
    let mut paths = BTreeMap::new();
    let selected = allowlist
        .iter()
        .flat_map(|(app, tools)| tools.iter().map(move |tool| (app, tool)));
    for (app, tool) in selected {
        let owners: Vec<_> = apps.iter().filter(|a| a.name == *app).collect();
        let [owner] = owners[..] else {
            return Err(format!("{} apps are named {app}", owners.len()));
        };
        let tools = owner.tools.iter().filter(|t| t.tool.name == *tool).count();
        let path = owner.uri.path();
        // Shared resource reads identify the app by its URI path alone.
        let shares_path = paths.insert(path, app).filter(|other| *other != app);
        let problem = [
            (tools != 1).then(|| format!("{app} has {tools} tools named {tool}")),
            shares_path.map(|other| format!("{other} and {app} share resource path {path}")),
            is_builtin(tool)
                .then(|| format!("{app}/{tool} conflicts with an enabled built-in tool")),
            exports
                .insert(tool.clone(), app.clone())
                .map(|other| format!("{tool} claimed by {other} and {app}")),
        ];
        problem.into_iter().flatten().next().map_or(Ok(()), Err)?;
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
