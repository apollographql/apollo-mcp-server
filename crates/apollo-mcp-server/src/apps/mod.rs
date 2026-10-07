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
