use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

/// Where to load the multi-graph manifest from.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
#[allow(dead_code, reason = "fields read by future Loading-state wiring")]
pub enum GraphsSource {
    /// Load the manifest from a YAML file on the local filesystem.
    Local { manifest: PathBuf },
    /// Pull an OCI image and read the manifest from one of its layers.
    Oci { image: String },
    /// Watch a directory of per-graph YAML files (one file per graph).
    /// Used for k8s ConfigMap volume mounts.
    LocalDir { dir: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_local_source() {
        let yaml = "source: local\nmanifest: ./graphs.yaml\n";
        let s: GraphsSource = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(s, GraphsSource::Local { .. }));
    }

    #[test]
    fn it_parses_oci_source() {
        let yaml = "source: oci\nimage: ghcr.io/acme/bundle:v1\n";
        let s: GraphsSource = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(s, GraphsSource::Oci { .. }));
    }

    #[test]
    fn it_deserializes_local_dir_source() {
        let yaml = "source: local_dir\ndir: /config/graphs";
        let src: GraphsSource = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(src, GraphsSource::LocalDir { dir } if dir == std::path::PathBuf::from("/config/graphs")));
    }
}
