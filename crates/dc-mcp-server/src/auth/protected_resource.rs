use serde::Serialize;
use url::Url;

use super::Config;

/// OAuth 2.1 Protected Resource Response
// TODO: This might be better found in an existing rust crate (or contributed upstream to one)
#[derive(Serialize)]
pub(super) struct ProtectedResource {
    /// The URL of the resource
    resource: Url,

    /// List of authorization servers protecting this resource
    authorization_servers: Vec<Url>,

    /// List of authentication methods allowed
    bearer_methods_supported: Vec<String>,

    /// Scopes allowed to request from the authorization servers
    scopes_supported: Vec<String>,

    /// Link to documentation about this resource
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_documentation: Option<Url>,
}

impl From<Config> for ProtectedResource {
    fn from(value: Config) -> Self {
        Self {
            resource: value.resource,
            authorization_servers: value.servers,
            bearer_methods_supported: vec!["header".to_string()], // The spec only supports header auth
            scopes_supported: value.scopes,
            resource_documentation: value.resource_documentation,
        }
    }
}
