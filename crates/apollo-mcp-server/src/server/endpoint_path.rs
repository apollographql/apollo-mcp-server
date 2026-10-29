use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;

/// The URL path that the Streamable HTTP transport serves MCP requests on, such as `/mcp`.
///
/// A valid path is absolute, is not the root (`/`), is not under `/.well-known`, and has
/// non-empty segments made only of letters, digits, `-`, `_`, `.`, and `~`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct EndpointPath(String);

/// Why a configured endpoint path was rejected.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EndpointPathError {
    #[error("path {0:?} must start with '/'")]
    NotAbsolute(String),

    #[error("serving at the root path '/' is not supported")]
    Root,

    #[error("path {0:?} must not be under /.well-known")]
    WellKnown(String),

    #[error(
        "path {path:?} has an invalid segment {segment:?}: segments must be non-empty, \
         must not be '.' or '..', and may only contain letters, digits, '-', '_', '.', and '~'"
    )]
    InvalidSegment { path: String, segment: String },
}

impl EndpointPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `path` is this endpoint path or a path below it.
    pub fn covers(&self, path: &str) -> bool {
        path.strip_prefix(self.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

impl Default for EndpointPath {
    fn default() -> Self {
        Self("/mcp".to_owned())
    }
}

impl fmt::Display for EndpointPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for EndpointPath {
    type Error = EndpointPathError;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        let Some(rest) = path.strip_prefix('/') else {
            return Err(EndpointPathError::NotAbsolute(path));
        };
        if rest.is_empty() {
            return Err(EndpointPathError::Root);
        }
        if rest == ".well-known" || rest.starts_with(".well-known/") {
            return Err(EndpointPathError::WellKnown(path));
        }
        if let Some(segment) = rest.split('/').find(|s| !is_valid_segment(s)) {
            let segment = segment.to_owned();
            return Err(EndpointPathError::InvalidSegment { path, segment });
        }
        Ok(Self(path))
    }
}

/// Restricting segments to URI unreserved characters keeps axum from reading them as
/// route parameters (`{id}`, `{*rest}`) and keeps paths free of percent-encoding.
fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[test]
    fn default_is_mcp() {
        assert_eq!(EndpointPath::default().as_str(), "/mcp");
    }

    #[rstest]
    #[case::single_segment("/api")]
    #[case::nested("/v1/services/mcp")]
    #[case::unreserved_characters("/my-service_v1.2~beta")]
    fn accepts_valid_path(#[case] path: &str) {
        let parsed = EndpointPath::try_from(path.to_owned()).unwrap();
        assert_eq!(parsed.as_str(), path);
    }

    #[rstest]
    #[case::relative("mcp", EndpointPathError::NotAbsolute("mcp".into()))]
    #[case::empty("", EndpointPathError::NotAbsolute("".into()))]
    #[case::root("/", EndpointPathError::Root)]
    #[case::well_known("/.well-known", EndpointPathError::WellKnown("/.well-known".into()))]
    #[case::under_well_known(
        "/.well-known/mcp",
        EndpointPathError::WellKnown("/.well-known/mcp".into())
    )]
    #[case::trailing_slash("/mcp/", invalid_segment("/mcp/", ""))]
    #[case::double_slash("/api//mcp", invalid_segment("/api//mcp", ""))]
    #[case::dot_segment("/api/./mcp", invalid_segment("/api/./mcp", "."))]
    #[case::dot_dot_segment("/api/../mcp", invalid_segment("/api/../mcp", ".."))]
    #[case::route_parameter("/{tenant}/mcp", invalid_segment("/{tenant}/mcp", "{tenant}"))]
    #[case::wildcard("/*rest", invalid_segment("/*rest", "*rest"))]
    #[case::query("/mcp?x=1", invalid_segment("/mcp?x=1", "mcp?x=1"))]
    #[case::percent_encoded("/my%20mcp", invalid_segment("/my%20mcp", "my%20mcp"))]
    fn rejects_invalid_path(#[case] path: &str, #[case] expected: EndpointPathError) {
        assert_eq!(EndpointPath::try_from(path.to_owned()), Err(expected));
    }

    fn invalid_segment(path: &str, segment: &str) -> EndpointPathError {
        EndpointPathError::InvalidSegment {
            path: path.to_owned(),
            segment: segment.to_owned(),
        }
    }

    #[rstest]
    #[case::exact("/mcp", true)]
    #[case::below("/mcp/health", true)]
    #[case::sibling_with_shared_prefix("/mcp-health", false)]
    #[case::unrelated("/health", false)]
    fn covers(#[case] path: &str, #[case] expected: bool) {
        assert_eq!(EndpointPath::default().covers(path), expected);
    }

    #[test]
    fn deserialize_rejects_invalid_path() {
        let err = serde_json::from_str::<EndpointPath>(r#""/""#).unwrap_err();
        assert!(err.to_string().contains("root path"));
    }
}
