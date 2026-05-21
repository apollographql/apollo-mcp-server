use std::path::{Path, PathBuf};

use super::types::Manifest;

#[derive(Debug, thiserror::Error)]
pub enum LocalLoadError {
    #[error("failed to read manifest file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse manifest YAML: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Load a manifest from a YAML file on disk. Relative file paths inside the
/// manifest (schema, operations) are resolved against the manifest's parent dir.
pub fn load_local(path: &Path) -> Result<Manifest, LocalLoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LocalLoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut manifest: Manifest = serde_yaml::from_str(&text)?;

    if let Some(parent) = path.parent() {
        for g in &mut manifest.graphs {
            if g.schema.is_relative() {
                let stripped = g.schema.strip_prefix("./").unwrap_or(g.schema.as_path());
                g.schema = parent.join(stripped);
            }
            for op in &mut g.operations {
                let op_path = PathBuf::from(&op);
                if op_path.is_relative() {
                    let stripped = op_path.strip_prefix("./").unwrap_or(op_path.as_path());
                    *op = parent.join(stripped).to_string_lossy().into_owned();
                }
            }
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn it_loads_and_resolves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("graphs.yaml");
        let mut f = std::fs::File::create(&manifest_path).unwrap();
        write!(
            f,
            "version: 1\n\
             graphs:\n\
             - name: a\n\
             \x20 endpoint: http://localhost:4000/\n\
             \x20 schema: ./a/schema.graphql\n\
             \x20 operations:\n\
             \x20   - ./a/ops/list.graphql\n"
        )
        .unwrap();
        drop(f);

        let manifest = load_local(&manifest_path).unwrap();
        let g = &manifest.graphs[0];
        assert_eq!(g.schema, dir.path().join("a/schema.graphql"));
        assert_eq!(
            g.operations[0],
            dir.path().join("a/ops/list.graphql").to_string_lossy()
        );
    }

    #[test]
    fn it_returns_an_error_when_file_missing() {
        let err = load_local(Path::new("/nonexistent/manifest.yaml")).unwrap_err();
        assert!(matches!(err, LocalLoadError::Read { .. }));
    }
}
