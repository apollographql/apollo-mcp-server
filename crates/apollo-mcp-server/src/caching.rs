//! Cache-hint configuration for MCP list/read responses.
//!
//! Governs the `ttlMs`/`cacheScope` cache hints (SEP-2549) attached to `tools/list`,
//! `resources/list`, `resources/read`, and `prompts/list` responses. These hints are only sent
//! to peers that negotiate MCP protocol version `2026-07-28` or later; older peers see no
//! change in behavior.

use rmcp::model::{CacheScope, ProtocolVersion};
use schemars::JsonSchema;
use serde::Deserialize;

/// Caching behavior for MCP list/read responses.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Caching {
    /// Time, in milliseconds, that `tools/list`, `resources/list`, `resources/read`, and
    /// `prompts/list` responses may be treated as fresh by clients.
    #[serde(default = "Caching::default_ttl_ms")]
    pub ttl_ms: u64,
}

impl Caching {
    fn default_ttl_ms() -> u64 {
        300_000
    }

    /// Applies the `ttlMs`/`cacheScope` cache hints to `result` when the negotiated protocol
    /// version supports them (SEP-2549, MCP `2026-07-28`+); older peers are left untouched.
    pub fn apply_to<T: CacheHints>(
        &self,
        result: &mut T,
        protocol_version: Option<&ProtocolVersion>,
    ) {
        if protocol_version.is_some_and(|v| *v >= ProtocolVersion::V_2026_07_28) {
            result.set_cache_hints(self.ttl_ms, CacheScope::Private);
        }
    }
}

impl Default for Caching {
    fn default() -> Self {
        Self {
            ttl_ms: Self::default_ttl_ms(),
        }
    }
}

/// Implemented by MCP list/read result types that carry SEP-2549 cache hints, so
/// [`Caching::apply_to`] can set them without duplicating the gating logic per call site.
pub trait CacheHints {
    fn set_cache_hints(&mut self, ttl_ms: u64, scope: CacheScope);
}

impl CacheHints for rmcp::model::ListToolsResult {
    fn set_cache_hints(&mut self, ttl_ms: u64, scope: CacheScope) {
        self.ttl_ms = Some(ttl_ms);
        self.cache_scope = Some(scope);
    }
}

impl CacheHints for rmcp::model::ListResourcesResult {
    fn set_cache_hints(&mut self, ttl_ms: u64, scope: CacheScope) {
        self.ttl_ms = Some(ttl_ms);
        self.cache_scope = Some(scope);
    }
}

impl CacheHints for rmcp::model::ListPromptsResult {
    fn set_cache_hints(&mut self, ttl_ms: u64, scope: CacheScope) {
        self.ttl_ms = Some(ttl_ms);
        self.cache_scope = Some(scope);
    }
}

impl CacheHints for rmcp::model::ReadResourceResult {
    fn set_cache_hints(&mut self, ttl_ms: u64, scope: CacheScope) {
        self.ttl_ms = Some(ttl_ms);
        self.cache_scope = Some(scope);
    }
}
