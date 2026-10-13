//! SEP-2243 `x-mcp-header` annotations on operation variables.
//!
//! An annotated tool parameter tells conforming Streamable HTTP clients to mirror its
//! value into an `Mcp-Param-{name}` request header, so intermediaries can route, meter,
//! or authorize on it without parsing the body.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use super::schema_walker::NotHeaderPrimitive;

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

/// RFC 9110 section 5.6.2 `tchar`.
fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

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
