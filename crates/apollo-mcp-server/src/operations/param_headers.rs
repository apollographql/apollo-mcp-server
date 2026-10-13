//! SEP-2243 `x-mcp-header` annotations on operation variables.
//!
//! An annotated tool parameter tells conforming Streamable HTTP clients to mirror its
//! value into an `Mcp-Param-{name}` request header, so intermediaries can route, meter,
//! or authorize on it without parsing the body.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use super::{Operation, schema_walker::NotHeaderPrimitive};
use crate::apps::App;

/// The JSON Schema keyword that marks a tool parameter for header mirroring.
pub(crate) const X_MCP_HEADER: &str = "x-mcp-header";

/// Header names for one operation's variables, keyed by variable name without the `$`.
///
/// Each name becomes the `{name}` in `Mcp-Param-{name}`. Names must be non-empty
/// RFC 9110 tokens and unique within the operation, ignoring case.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ParamHeaders(HashMap<String, String>);

impl ParamHeaders {
    /// The header name for `variable`, if it is annotated.
    pub(crate) fn get(&self, variable: &str) -> Option<&str> {
        self.0.get(variable).map(String::as_str)
    }

    /// The annotated variable names.
    pub(crate) fn variables(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

/// A configured header name that the specification does not allow.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InvalidParamHeader {
    #[error("header name for variable `{variable}` must not be empty")]
    Empty { variable: String },

    #[error("header name `{header}` for variable `{variable}` is not a valid HTTP token")]
    NotAToken { variable: String, header: String },

    #[error(
        "header name `{header}` is used by more than one variable (names are case-insensitive)"
    )]
    Duplicate { header: String },
}

/// An annotated variable that can't become an `Mcp-Param-*` header.
#[derive(Debug, thiserror::Error, PartialEq)]
#[error("variable `${variable}` can't be mirrored into a header: {reason}")]
pub struct UnmirrorableVariable {
    pub variable: String,
    pub reason: UnmirrorableReason,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum UnmirrorableReason {
    #[error("the operation declares no such variable")]
    Undeclared,

    #[error("the operation's `variables` fix its value, so it is not a tool parameter")]
    Fixed,

    #[error(transparent)]
    Type(#[from] NotHeaderPrimitive),
}

impl TryFrom<HashMap<String, String>> for ParamHeaders {
    type Error = InvalidParamHeader;

    fn try_from(headers: HashMap<String, String>) -> Result<Self, Self::Error> {
        let mut seen = HashSet::new();
        for (variable, header) in &headers {
            if header.is_empty() {
                return Err(InvalidParamHeader::Empty {
                    variable: variable.clone(),
                });
            }
            // Token characters exclude CR, LF, and every other control character.
            if !header.bytes().all(is_tchar) {
                return Err(InvalidParamHeader::NotAToken {
                    variable: variable.clone(),
                    header: header.clone(),
                });
            }
            if !seen.insert(header.to_ascii_lowercase()) {
                return Err(InvalidParamHeader::Duplicate {
                    header: header.clone(),
                });
            }
        }
        Ok(Self(headers))
    }
}

impl<'de> Deserialize<'de> for ParamHeaders {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let headers = HashMap::<String, String>::deserialize(deserializer)?;
        Self::try_from(headers).map_err(serde::de::Error::custom)
    }
}

/// Warns about each configured operation that isn't loaded. Its headers are neither
/// annotated nor checked, which an operator relying on them would otherwise miss.
pub(crate) fn warn_unmatched_operations(
    param_headers: &HashMap<String, ParamHeaders>,
    operations: &[Operation],
) {
    for operation in unmatched_operations(param_headers, operations) {
        tracing::warn!(
            operation,
            "param_headers names an operation that isn't loaded, so its headers aren't enforced"
        );
    }
}

fn unmatched_operations<'a>(
    param_headers: &'a HashMap<String, ParamHeaders>,
    operations: &[Operation],
) -> Vec<&'a str> {
    let loaded: HashSet<&str> = operations
        .iter()
        .map(|operation| operation.as_ref().name.as_ref())
        .collect();
    let mut unmatched: Vec<&str> = param_headers
        .keys()
        .map(String::as_str)
        .filter(|name| !loaded.contains(name))
        .collect();
    unmatched.sort_unstable();
    unmatched
}

/// Warns about each configured operation that shares its name with an app tool.
///
/// In app mode `call_tool` runs the app tool, but rmcp's `get_tool` has no request
/// context and checks the operation's annotations, so every such call fails with
/// `-32020`.
pub(crate) fn warn_app_tool_collisions(
    param_headers: &HashMap<String, ParamHeaders>,
    apps: &[App],
) {
    for (app, tool) in app_tool_collisions(param_headers, apps) {
        tracing::warn!(
            app,
            tool,
            "param_headers names an operation that shares its name with an app tool, so app-mode calls to that tool fail with -32020"
        );
    }
}

pub(crate) fn app_tool_collisions<'a>(
    param_headers: &HashMap<String, ParamHeaders>,
    apps: &'a [App],
) -> Vec<(&'a str, &'a str)> {
    apps.iter()
        .flat_map(|app| {
            app.tools
                .iter()
                .map(move |tool| (app.name.as_str(), tool.tool.name.as_ref()))
        })
        .filter(|(_, tool)| param_headers.contains_key(*tool))
        .collect()
}

/// RFC 9110 section 5.6.2 `tchar`.
fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use rstest::rstest;

    use super::*;
    use crate::operations::{MutationMode, RawOperation};

    #[test]
    fn unmatched_operations_lists_configured_names_that_are_not_loaded() {
        let schema = Schema::parse_and_validate("type Query { id: ID }", "schema.graphql").unwrap();
        let loaded = RawOperation::from(("query GetId { id }".to_string(), None))
            .into_operation(
                &schema,
                None,
                MutationMode::None,
                false,
                false,
                false,
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
            .unwrap()
            .unwrap();
        let param_headers = HashMap::from([
            ("GetId".to_string(), ParamHeaders::default()),
            ("GetWorkFlow".to_string(), ParamHeaders::default()),
        ]);

        assert_eq!(
            unmatched_operations(&param_headers, &[loaded]),
            vec!["GetWorkFlow"]
        );
    }

    fn parse(json: serde_json::Value) -> Result<ParamHeaders, serde_json::Error> {
        serde_json::from_value(json)
    }

    #[test]
    fn deserialize_accepts_token_header_names() {
        let headers =
            parse(serde_json::json!({ "handle": "Workflow-Handle", "region": "x_1~" })).unwrap();

        assert_eq!(headers.get("handle"), Some("Workflow-Handle"));
    }

    #[rstest]
    #[case::empty("")]
    #[case::space("Workflow Handle")]
    #[case::colon("bad:name")]
    #[case::crlf("Region\r\nX-Injected")]
    #[case::non_ascii("Régión")]
    fn deserialize_rejects_non_token_header_names(#[case] header: &str) {
        assert!(parse(serde_json::json!({ "handle": header })).is_err());
    }

    #[test]
    fn deserialize_rejects_case_insensitive_duplicates() {
        let error = parse(serde_json::json!({ "a": "Region", "b": "region" })).unwrap_err();

        assert!(error.to_string().contains("more than one variable"));
    }
}
