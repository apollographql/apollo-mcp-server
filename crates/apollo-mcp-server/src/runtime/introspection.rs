use schemars::JsonSchema;
use serde::Deserialize;

/// Introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Introspection {
    /// Execution configuration for introspection
    pub execute: ExecuteConfig,

    /// Introspect configuration for allowing clients to run introspection
    pub introspect: IntrospectConfig,

    /// Search tool configuration
    pub search: SearchConfig,

    /// Validate configuration for checking operations before execution
    pub validate: ValidateConfig,
}

/// Execution-specific introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ExecuteConfig {
    /// Enable introspection for execution
    pub enabled: bool,
    /// Optional custom hint appended to the execute tool description
    pub hint: Option<String>,
}

/// Introspect-specific introspection configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IntrospectConfig {
    /// Enable introspection requests
    pub enabled: bool,

    /// Minify introspection results
    pub minify: bool,
    /// Optional custom hint appended to the introspect tool description
    pub hint: Option<String>,
}

/// Search tool configuration
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    /// Enable search tool
    pub enabled: bool,

    /// The amount of memory used for indexing (in bytes)
    pub index_memory_bytes: usize,

    /// The depth of subtype information to include from matching types
    /// (1 is just the matching type, 2 is the matching type plus the types it references, etc.
    /// Defaults to 1.)
    pub leaf_depth: usize,

    /// Minify search results
    pub minify: bool,
    /// Optional custom hint appended to the search tool description
    pub hint: Option<String>,
    /// Default number of results when the caller omits `limit`.
    pub default_limit: usize,
    /// Hard cap on the number of results the caller may request.
    pub max_limit: usize,
    /// Return-type flatten depth used to enrich each operation's index document.
    pub flatten_depth: usize,
    /// Semantic (vector) search settings.
    pub semantic: SemanticConfig,
    /// Hybrid fusion settings.
    pub hybrid: HybridConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            index_memory_bytes: 50_000_000,
            leaf_depth: 1,
            minify: false,
            hint: None,
            default_limit: 10,
            max_limit: 50,
            flatten_depth: 2,
            semantic: SemanticConfig::default(),
            hybrid: HybridConfig::default(),
        }
    }
}

/// Semantic (vector) search configuration, nested under `search`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticConfig {
    /// Enable semantic (vector) search fused with lexical BM25. Only takes
    /// effect when the search tool itself is enabled. If the embedder fails
    /// to initialize, the tool degrades to lexical-only.
    pub enabled: bool,
    /// Embedding model name (e.g. "bge-small-en-v1.5").
    pub model: String,
    /// ONNX intra-op thread count for inference (keep small to respect CPU limits).
    pub inference_threads: usize,
    /// Path to a SQLite file used to cache operation embeddings across restarts.
    /// Unset = disabled (embed on every start). Relative paths resolve against CWD.
    #[serde(default)]
    pub cache_path: Option<std::path::PathBuf>,
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "bge-small-en-v1.5".to_string(),
            inference_threads: 1,
            cache_path: None,
        }
    }
}

/// Hybrid fusion configuration, nested under `search`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HybridConfig {
    /// Reciprocal Rank Fusion constant `k`. Larger = flatter rank weighting.
    pub rrf_k: f32,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self { rrf_k: 60.0 }
    }
}

/// Validation tool configuration
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ValidateConfig {
    /// Enable validation tool
    pub enabled: bool,
    /// Optional custom hint appended to the validate tool description
    pub hint: Option<String>,
}

impl Introspection {
    /// Check if any introspection tools are enabled
    pub fn any_enabled(&self) -> bool {
        self.execute.enabled | self.introspect.enabled | self.search.enabled | self.validate.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_config_defaults() {
        let c = SearchConfig::default();
        assert_eq!(c.default_limit, 10);
        assert_eq!(c.max_limit, 50);
        assert_eq!(c.flatten_depth, 2);
        assert!(c.semantic.enabled);
        assert_eq!(c.semantic.model, "bge-small-en-v1.5");
        assert_eq!(c.semantic.inference_threads, 1);
        assert_eq!(c.hybrid.rrf_k, 60.0);
    }

    #[test]
    fn semantic_cache_path_parses() {
        let c: SemanticConfig = serde_yaml::from_str(
            "enabled: true\nmodel: bge-small-en-v1.5\ninference_threads: 2\ncache_path: /data/emb.db\n",
        )
        .unwrap();
        assert_eq!(c.cache_path, Some(std::path::PathBuf::from("/data/emb.db")));
    }
}
