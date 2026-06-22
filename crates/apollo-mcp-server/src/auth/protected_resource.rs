use serde::Serialize;
use url::Url;

use super::Config;

/// OAuth 2.1 Protected Resource Response
#[derive(Serialize)]
pub(super) struct ProtectedResource {
    /// The URL of the resource
    resource: Url,

    /// List of authorization servers protecting this resource.
    authorization_servers: Vec<String>,

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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::bare_authority_no_slash("https://auth.example.com")]
    #[case::bare_authority_with_slash("https://auth.example.com/")]
    #[case::path_no_slash("https://auth.example.com/realms/main")]
    #[case::path_with_slash("https://auth.example.com/realms/main/")]
    fn authorization_servers_preserves_raw_input(#[case] raw: &str) {
        let yaml = format!(
            r#"
                servers:
                  - "{raw}"
                audiences:
                  - test-audience
                resource: https://mcp.example.com/mcp
                scopes:
                  - read
            "#
        );

        let config: Config = serde_yaml::from_str(&yaml).expect("config parses");
        let metadata = ProtectedResource::from(config);
        let json = serde_json::to_value(&metadata).expect("metadata serializes");

        assert_eq!(json["authorization_servers"], serde_json::json!([raw]));
    }

    #[test]
    fn multiple_servers_all_preserved() {
        let yaml = r#"
            servers:
              - https://issuer-a.example.com
              - https://issuer-b.example.com/
              - https://issuer-c.example.com/realms/main
            audiences:
              - test-audience
            resource: https://mcp.example.com/mcp
            scopes:
              - read
        "#;

        let config: Config = serde_yaml::from_str(yaml).expect("config parses");
        let metadata = ProtectedResource::from(config);
        let json = serde_json::to_value(&metadata).expect("metadata serializes");

        assert_eq!(
            json["authorization_servers"],
            serde_json::json!([
                "https://issuer-a.example.com",
                "https://issuer-b.example.com/",
                "https://issuer-c.example.com/realms/main",
            ])
        );
    }
}
