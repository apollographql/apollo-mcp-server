use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use url::Url;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct UpstreamAuthConfig {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub token_url: String,
    pub client_id_env: String,
    pub client_secret_env: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub graphs: Vec<GraphConfig>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    pub name: String,
    #[schemars(schema_with = "Url::json_schema")]
    pub endpoint: Url,
    pub schema: PathBuf,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub upstream_auth: Option<UpstreamAuthConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_a_minimal_manifest() {
        let yaml = r#"
            version: 1
            graphs:
              - name: a
                endpoint: http://localhost:4000/
                schema: ./a/schema.graphql
        "#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.version, 1);
        assert_eq!(m.graphs.len(), 1);
        assert_eq!(m.graphs[0].name, "a");
        assert_eq!(m.graphs[0].operations.len(), 0);
    }

    #[test]
    fn it_rejects_unknown_fields() {
        let yaml = r#"
            version: 1
            graphs: []
            extra: field
        "#;
        let result: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn it_requires_name_endpoint_schema() {
        let yaml = r#"
            version: 1
            graphs:
              - endpoint: http://x/
        "#;
        let result: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn it_parses_upstream_auth() {
        let yaml = r#"
            version: 1
            graphs:
              - name: athena
                endpoint: http://localhost:4000/
                schema: ./schema.graphql
                upstream_auth:
                  type: oauth2_client_credentials
                  token_url: "https://api.preview.platform.athenahealth.com/oauth2/v1/token"
                  client_id_env: ATHENA_PREVIEW_CREDS_CLIENT_ID
                  client_secret_env: ATHENA_PREVIEW_CREDS_CLIENT_SECRET
        "#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let auth = m.graphs[0].upstream_auth.as_ref().unwrap();
        assert_eq!(auth.token_url, "https://api.preview.platform.athenahealth.com/oauth2/v1/token");
        assert_eq!(auth.client_id_env, "ATHENA_PREVIEW_CREDS_CLIENT_ID");
        assert_eq!(auth.client_secret_env, "ATHENA_PREVIEW_CREDS_CLIENT_SECRET");
    }

    #[test]
    fn it_parses_without_upstream_auth() {
        let yaml = r#"
            version: 1
            graphs:
              - name: petstore
                endpoint: http://localhost:4000/
                schema: ./schema.graphql
        "#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.graphs[0].upstream_auth.is_none());
    }
}
