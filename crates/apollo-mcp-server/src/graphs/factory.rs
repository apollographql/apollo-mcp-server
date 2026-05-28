use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::Schema;
use apollo_schema_index::{OperationType, SchemaIndex};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::RwLock;

use super::manifest::GraphConfig;
use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::OperationError;
use crate::headers::ForwardHeaders;
use crate::operations::{AnnotationOverrides, MutationMode, Operation, RawOperation};

use super::context::GraphContext;
use super::credentials::default_provider;

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("failed to read schema file for graph '{graph}': {source}")]
    ReadSchema {
        graph: String,
        source: std::io::Error,
    },
    #[error("schema parse/validate failed for graph '{graph}': {message}")]
    InvalidSchema { graph: String, message: String },
    #[error("failed to read operation path {path} for graph '{graph}': {source}")]
    ReadOp {
        graph: String,
        path: String,
        source: std::io::Error,
    },
    #[error("invalid operation in graph '{graph}': {source}")]
    InvalidOp {
        graph: String,
        #[source]
        source: OperationError,
    },
    #[error("invalid header in graph '{graph}': {message}")]
    BadHeader { graph: String, message: String },
    #[error("failed to build search index for graph '{graph}': {source}")]
    Index {
        graph: String,
        #[source]
        source: apollo_schema_index::error::IndexingError,
    },
}

#[expect(clippy::too_many_arguments)]
pub async fn build_graph_context(
    config: GraphConfig,
    index_memory_bytes: usize,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
    enable_output_schema: bool,
    annotation_overrides: &HashMap<String, AnnotationOverrides>,
    description_overrides: &HashMap<String, String>,
    custom_scalar_map: Option<CustomScalarMap>,
    base_headers: &HeaderMap,
    base_forward_headers: &ForwardHeaders,
) -> Result<GraphContext, BuildError> {
    let schema_text =
        std::fs::read_to_string(&config.schema).map_err(|source| BuildError::ReadSchema {
            graph: config.name.clone(),
            source,
        })?;

    let parsed = Schema::parse(schema_text, "schema.graphql")
        .and_then(|s| s.validate())
        .map_err(|e| BuildError::InvalidSchema {
            graph: config.name.clone(),
            message: e.to_string(),
        })?;

    let root_types = match mutation_mode {
        MutationMode::None => OperationType::Query.into(),
        _ => OperationType::Query | OperationType::Mutation,
    };
    let index = SchemaIndex::new(&parsed, root_types, index_memory_bytes).map_err(|source| {
        BuildError::Index {
            graph: config.name.clone(),
            source,
        }
    })?;

    let raw_ops = load_raw_operations(&config)?;

    let mut operations: Vec<Operation> = Vec::new();
    for raw in raw_ops {
        let op = raw
            .into_operation_with_prefix(
                &parsed,
                custom_scalar_map.as_ref(),
                mutation_mode,
                disable_type_description,
                disable_schema_description,
                enable_output_schema,
                annotation_overrides,
                description_overrides,
                Some(&config.name),
            )
            .map_err(|source| BuildError::InvalidOp {
                graph: config.name.clone(),
                source,
            })?;
        if let Some(op) = op {
            operations.push(op);
        }
    }

    let mut headers = base_headers.clone();
    for (k, v) in &config.headers {
        let name = HeaderName::from_str(k).map_err(|e| BuildError::BadHeader {
            graph: config.name.clone(),
            message: e.to_string(),
        })?;
        let value = HeaderValue::from_str(v).map_err(|e| BuildError::BadHeader {
            graph: config.name.clone(),
            message: e.to_string(),
        })?;
        headers.insert(name, value);
    }

    Ok(GraphContext {
        name: config.name,
        schema: Arc::new(RwLock::new(parsed)),
        endpoint: config.endpoint,
        headers,
        forward_headers: base_forward_headers.clone(),
        operations: Arc::new(RwLock::new(operations)),
        search_index: index,
        mutation_mode,
        custom_scalar_map,
        credentials: default_provider(),
    })
}

fn load_raw_operations(config: &GraphConfig) -> Result<Vec<RawOperation>, BuildError> {
    let mut raw: Vec<RawOperation> = Vec::new();
    for entry in &config.operations {
        let path = Path::new(entry.as_str());
        if path.is_dir() {
            let dir = std::fs::read_dir(path).map_err(|source| BuildError::ReadOp {
                graph: config.name.clone(),
                path: entry.clone(),
                source,
            })?;
            for child in dir.flatten() {
                let child_path = child.path();
                if child_path.extension().and_then(|e| e.to_str()) == Some("graphql") {
                    let body = std::fs::read_to_string(&child_path).map_err(|source| {
                        BuildError::ReadOp {
                            graph: config.name.clone(),
                            path: child_path.display().to_string(),
                            source,
                        }
                    })?;
                    raw.push(RawOperation::from((
                        body,
                        Some(child_path.display().to_string()),
                    )));
                }
            }
        } else {
            let body = std::fs::read_to_string(path).map_err(|source| BuildError::ReadOp {
                graph: config.name.clone(),
                path: entry.clone(),
                source,
            })?;
            raw.push(RawOperation::from((body, Some(entry.clone()))));
        }
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[tokio::test]
    async fn it_builds_context_with_one_operation() {
        let dir = tempfile::tempdir().unwrap();
        let schema = write_file(dir.path(), "schema.graphql", "type Query { id: String }");
        let op = write_file(dir.path(), "op.graphql", "query GetId { id }");

        let config = GraphConfig {
            name: "g".into(),
            endpoint: url::Url::parse("http://localhost:4000/").unwrap(),
            schema,
            operations: vec![op.display().to_string()],
            headers: HashMap::new(),
            upstream_auth: None,
        };

        let ctx = build_graph_context(
            config,
            15_000_000,
            MutationMode::None,
            false,
            false,
            false,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HeaderMap::new(),
            &vec![],
        )
        .await
        .unwrap();

        let ops = ctx.operations.read().await;
        assert_eq!(ops.len(), 1);
        let tool: &rmcp::model::Tool = ops[0].as_ref();
        assert_eq!(tool.name.as_ref(), "g__GetId");
    }

    #[tokio::test]
    async fn it_loads_operations_from_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "schema.graphql", "type Query { id: String }");
        let ops_dir = dir.path().join("ops");
        std::fs::create_dir(&ops_dir).unwrap();
        write_file(&ops_dir, "a.graphql", "query GetA { id }");
        write_file(&ops_dir, "b.graphql", "query GetB { id }");
        write_file(&ops_dir, "skip.txt", "this should be skipped");

        let config = GraphConfig {
            name: "g".into(),
            endpoint: url::Url::parse("http://localhost:4000/").unwrap(),
            schema: dir.path().join("schema.graphql"),
            operations: vec![ops_dir.display().to_string()],
            headers: HashMap::new(),
            upstream_auth: None,
        };

        let ctx = build_graph_context(
            config,
            15_000_000,
            MutationMode::None,
            false,
            false,
            false,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HeaderMap::new(),
            &vec![],
        )
        .await
        .unwrap();

        let ops = ctx.operations.read().await;
        assert_eq!(ops.len(), 2);
    }
}
