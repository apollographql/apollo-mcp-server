//! AIR-399: search-quality baseline for the `search` tool.
//!
//! This module pins "search query → expected top-k results" fixtures captured against
//! *today's* search implementation over the offline catalog fixture
//! (`testdata/search_baseline/catalog.graphql`). The checked-in fixture
//! (`testdata/search_baseline/baseline.json`) is the parity gate the Discovery search
//! migration (S2.5) must match or beat, and the floor for later retrieval experiments.
//!
//! The fixture is *captured*, never hand-edited. To re-capture (only when the catalog
//! fixture or the search implementation intentionally changes):
//!
//! ```sh
//! cargo test -p apollo-mcp-server capture_search_baseline -- --ignored
//! ```
//!
//! Coverage spans unscoped queries, service-scoped queries, and the known-hard cases:
//! short natural-language queries against service-prefixed type names.

use std::sync::Arc;

use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use apollo_schema_index::{OperationType, Options, SchemaIndex};
use rmcp::model::ContentBlock;
use rmcp::serde_json;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::search::{Input, MAX_SEARCH_RESULTS, Search};

const CATALOG: &str = include_str!("testdata/search_baseline/catalog.graphql");

/// Path of the checked-in baseline fixture (read and written in the source tree so that
/// capture mode updates the committed file).
const BASELINE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/introspection/tools/testdata/search_baseline/baseline.json"
);

/// The fixed query set. Each entry is (id, coverage, terms).
///
/// Coverage labels:
/// * `unscoped` — plain domain terms with no service qualifier
/// * `scoped`   — terms qualified by a service prefix (or exact prefixed names)
/// * `hard`     — short natural-language queries that must land on service-prefixed names
const QUERIES: &[(&str, &str, &[&str])] = &[
    // Unscoped
    ("unscoped-invoice", "unscoped", &["invoice"]),
    ("unscoped-product", "unscoped", &["product"]),
    ("unscoped-order", "unscoped", &["order"]),
    ("unscoped-customer-email", "unscoped", &["customer email"]),
    // Scoped
    ("scoped-billing-invoice", "scoped", &["billing invoice"]),
    (
        "scoped-inventory-product-exact",
        "scoped",
        &["Inventory_Product"],
    ),
    (
        "scoped-support-ticket-status",
        "scoped",
        &["support ticket status"],
    ),
    (
        "scoped-accounts-reset-password-field",
        "scoped",
        &["accounts_resetPassword"],
    ),
    (
        "scoped-shipping-carrier-multi-term",
        "scoped",
        &["shipping", "carrier"],
    ),
    // Known-hard: short natural language vs. prefixed names
    ("hard-track-my-package", "hard", &["track my package"]),
    ("hard-refund-a-payment", "hard", &["refund a payment"]),
    ("hard-reset-password", "hard", &["reset password"]),
    ("hard-out-of-stock", "hard", &["out of stock"]),
    ("hard-open-ticket-multi-term", "hard", &["open", "ticket"]),
];

/// Parameters the baseline was captured with. These mirror how the server constructs the
/// search tool (see `starting.rs`) with `mutation_mode: all` and default search config.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct CaptureParams {
    allow_mutations: bool,
    leaf_depth: usize,
    index_memory_bytes: usize,
    minify: bool,
    max_type_matches: usize,
    max_paths_per_type: usize,
    short_path_boost_factor: f32,
    parent_match_boost_factor: f32,
}

impl Default for CaptureParams {
    fn default() -> Self {
        let index_options = Options::default();
        Self {
            allow_mutations: true,
            leaf_depth: 1,
            index_memory_bytes: 50_000_000,
            minify: false,
            max_type_matches: index_options.max_type_matches,
            max_paths_per_type: index_options.max_paths_per_type,
            short_path_boost_factor: index_options.short_path_boost_factor,
            parent_match_boost_factor: index_options.parent_match_boost_factor,
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct QueryBaseline {
    id: String,
    coverage: String,
    terms: Vec<String>,
    /// The top-k root paths returned by the schema index, in rank order. This is the
    /// ranked result list the search tool selects results from.
    top_paths: Vec<String>,
    /// The sorted, deduplicated set of type definitions returned by the MCP `search`
    /// tool for these terms. This is what an MCP client observes end-to-end, and what
    /// the AIR-399 offline serve-smoke re-checks against a running server.
    result_types: Vec<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Baseline {
    description: String,
    captured_with: CaptureParams,
    k: usize,
    queries: Vec<QueryBaseline>,
}

fn catalog_schema() -> Valid<Schema> {
    Schema::parse(CATALOG, "catalog.graphql")
        .expect("Failed to parse catalog fixture")
        .validate()
        .expect("Failed to validate catalog fixture")
}

/// Extract the names of all type definitions in a search result content block.
fn type_names_in_block(sdl: &str) -> Vec<String> {
    let doc = apollo_compiler::ast::Document::parse(sdl, "block.graphql")
        .expect("search result block is not valid SDL");
    doc.definitions
        .iter()
        .filter_map(|def| def.name())
        .map(|name| name.to_string())
        .collect()
}

/// Run today's search over the catalog fixture and capture the baseline for all queries.
async fn capture_baseline() -> Baseline {
    let params = CaptureParams::default();
    let schema = catalog_schema();
    let index = SchemaIndex::new(
        &schema,
        OperationType::Query | OperationType::Mutation,
        params.index_memory_bytes,
    )
    .expect("Failed to index catalog fixture");

    let schema = Arc::new(RwLock::new(catalog_schema()));
    let search = Search::new(
        schema,
        params.allow_mutations,
        params.leaf_depth,
        params.index_memory_bytes,
        params.minify,
        None,
    )
    .expect("Failed to create search tool");

    let mut queries = Vec::with_capacity(QUERIES.len());
    for (id, coverage, terms) in QUERIES {
        let terms: Vec<String> = terms.iter().map(ToString::to_string).collect();

        // The ranked top-k root paths, exactly as the search tool selects them.
        let mut paths = index
            .search(terms.clone(), Options::default())
            .expect("Index search failed");
        paths.truncate(MAX_SEARCH_RESULTS);
        let top_paths = paths.iter().map(|p| p.inner.to_string()).collect();

        // The end-to-end MCP tool result: the set of type definitions returned.
        let input: Input = serde_json::from_value(serde_json::json!({ "terms": terms }))
            .expect("Failed to build search input");
        let result = search
            .execute(input)
            .await
            .expect("Search execution failed");
        let mut result_types: Vec<String> = result
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .flat_map(|text| type_names_in_block(&text))
            .collect();
        result_types.sort();
        result_types.dedup();

        queries.push(QueryBaseline {
            id: (*id).to_string(),
            coverage: (*coverage).to_string(),
            terms,
            top_paths,
            result_types,
        });
    }

    Baseline {
        description: "AIR-399 search-quality baseline: expected top-k results of today's \
                      apollo-mcp-server search over the offline catalog fixture. Parity gate \
                      for the Discovery search migration (S2.5). Captured, not hand-authored; \
                      re-capture with: cargo test -p apollo-mcp-server capture_search_baseline \
                      -- --ignored"
            .to_string(),
        captured_with: params,
        k: MAX_SEARCH_RESULTS,
        queries,
    }
}

fn read_committed_baseline() -> Baseline {
    let raw = std::fs::read_to_string(BASELINE_PATH).unwrap_or_else(|e| {
        panic!(
            "Failed to read {BASELINE_PATH}: {e}. Capture it with \
             cargo test -p apollo-mcp-server capture_search_baseline -- --ignored"
        )
    });
    serde_json::from_str(&raw).expect("baseline.json is not valid")
}

/// Capture mode: rewrite the checked-in fixture from today's search results.
/// Ignored by default so `cargo test` never silently regenerates the gate; run explicitly
/// with `cargo test -p apollo-mcp-server capture_search_baseline -- --ignored`.
#[tokio::test]
#[ignore = "capture mode: rewrites testdata/search_baseline/baseline.json"]
async fn capture_search_baseline() {
    let current = capture_baseline().await;
    let mut serialized =
        serde_json::to_string_pretty(&current).expect("Failed to serialize baseline");
    serialized.push('\n');
    std::fs::write(BASELINE_PATH, serialized).expect("Failed to write baseline.json");
    println!("Re-captured search baseline at {BASELINE_PATH}");
}

/// The parity gate: today's search must reproduce the checked-in baseline exactly.
#[tokio::test]
async fn baseline_reproduces_todays_search() {
    let current = capture_baseline().await;
    let committed = read_committed_baseline();

    assert_eq!(
        committed.captured_with, current.captured_with,
        "Search capture parameters changed; re-capture the baseline if this is intentional"
    );
    assert_eq!(committed.k, current.k, "top-k changed (MAX_SEARCH_RESULTS)");

    let committed_ids: Vec<&str> = committed.queries.iter().map(|q| q.id.as_str()).collect();
    let current_ids: Vec<&str> = current.queries.iter().map(|q| q.id.as_str()).collect();
    assert_eq!(
        committed_ids, current_ids,
        "Baseline query set drifted from QUERIES; re-capture the baseline"
    );

    for (committed_query, current_query) in committed.queries.iter().zip(current.queries.iter()) {
        assert_eq!(
            committed_query,
            current_query,
            "Search results for baseline query '{}' (terms {:?}) no longer match the \
             checked-in AIR-399 baseline.\n\
             expected top-{} paths: {:#?}\n\
             actual   top-{} paths: {:#?}\n\
             expected result types: {:?}\n\
             actual   result types: {:?}\n\
             If this change to search behavior is intentional, re-capture with \
             AIR399_UPDATE_SEARCH_BASELINE=1 cargo test -p apollo-mcp-server search_baseline \
             and include the fixture diff in review.",
            committed_query.id,
            committed_query.terms,
            committed.k,
            committed_query.top_paths,
            current.k,
            current_query.top_paths,
            committed_query.result_types,
            current_query.result_types,
        );
    }
}

/// Guard the required coverage classes: unscoped, scoped, and the known-hard
/// short-natural-language-vs-prefixed-names cases must all be represented.
#[test]
fn baseline_covers_required_query_classes() {
    let committed = read_committed_baseline();
    for class in ["unscoped", "scoped", "hard"] {
        let count = committed
            .queries
            .iter()
            .filter(|q| q.coverage == class)
            .count();
        assert!(
            count >= 3,
            "Baseline must keep at least 3 '{class}' queries, found {count}"
        );
    }
    // Every query must pin a non-empty expectation — an empty expectation cannot gate parity.
    for q in &committed.queries {
        assert!(
            !q.top_paths.is_empty(),
            "Baseline query '{}' has no expected paths",
            q.id
        );
        assert!(
            !q.result_types.is_empty(),
            "Baseline query '{}' has no expected result types",
            q.id
        );
    }
}
