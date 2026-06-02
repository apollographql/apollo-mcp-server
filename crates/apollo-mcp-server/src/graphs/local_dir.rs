use serde::Deserialize;
use url::Url;

/// Config for a single graph, loaded from a per-graph YAML file in the watched directory.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerGraphFileConfig {
    pub name: String,
    pub endpoint: Url,
    /// OCI reference for the schema SDL in Zot, e.g.
    /// `zot.zot.svc.cluster.local:5050/schemas/graph-A:abc123`
    pub schema_ref: String,
    pub sha: String,
}

pub fn parse_per_graph_config(yaml: &str) -> Result<PerGraphFileConfig, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_a_valid_per_graph_config() {
        let yaml = r#"
            name: graph-A
            endpoint: http://graph-A.graph-A.svc.cluster.local:4000/graphql
            schema_ref: zot.zot.svc.cluster.local:5050/schemas/graph-A:abc123
            sha: abc123
        "#;
        let cfg = parse_per_graph_config(yaml).unwrap();
        assert_eq!(cfg.name, "graph-A");
        assert_eq!(cfg.sha, "abc123");
        assert_eq!(cfg.schema_ref, "zot.zot.svc.cluster.local:5050/schemas/graph-A:abc123");
    }

    #[test]
    fn it_rejects_unknown_fields() {
        let yaml = r#"
            name: graph-A
            endpoint: http://localhost:4000/graphql
            schema_ref: zot:5050/schemas/graph-A:abc
            sha: abc
            unknown_field: oops
        "#;
        assert!(parse_per_graph_config(yaml).is_err());
    }

    #[test]
    fn it_rejects_missing_sha() {
        let yaml = r#"
            name: graph-A
            endpoint: http://localhost:4000/graphql
            schema_ref: zot:5050/schemas/graph-A:abc
        "#;
        assert!(parse_per_graph_config(yaml).is_err());
    }
}
