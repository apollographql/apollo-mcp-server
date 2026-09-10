//! Cache-hint configuration for MCP list/read responses.
//!
//! Governs the `ttlMs`/`cacheScope` cache hints (SEP-2549) attached to `tools/list`,
//! `resources/list`, `resources/read`, and `prompts/list` responses. These hints are only sent
//! to peers that negotiate MCP protocol version `2026-07-28` or later; older peers see no
//! change in behavior.

use rmcp::model::{CacheScope, ProtocolVersion};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::server::states::MAX_SUPPORTED_PROTOCOL_VERSION;

/// Caching behavior for MCP list/read responses.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Caching {
    /// Time, in milliseconds, that `tools/list`, `resources/list`, `resources/read`, and
    /// `prompts/list` responses may be treated as fresh by clients.
    #[schemars(default = "Caching::default_ttl_ms")]
    pub ttl_ms: u64,
}

impl Caching {
    fn default_ttl_ms() -> u64 {
        300_000
    }

    /// Applies the `ttlMs`/`cacheScope` cache hints to `result` when the negotiated protocol
    /// version supports them (SEP-2549, MCP `2026-07-28`+) and is within this server's
    /// supported range. A peer's claim alone cannot enable an unsupported revision.
    pub(crate) fn apply_to<T: CacheHints>(
        &self,
        result: T,
        protocol_version: Option<&ProtocolVersion>,
    ) -> T {
        if supports_cache_hints(protocol_version, &MAX_SUPPORTED_PROTOCOL_VERSION) {
            result.with_cache_hints(self.ttl_ms, CacheScope::Private)
        } else {
            result
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
pub(crate) trait CacheHints {
    fn with_cache_hints(self, ttl_ms: u64, scope: CacheScope) -> Self;
}

fn supports_cache_hints(peer: Option<&ProtocolVersion>, server_max: &ProtocolVersion) -> bool {
    peer.is_some_and(|version| *version >= ProtocolVersion::V_2026_07_28 && version <= server_max)
}

macro_rules! impl_cache_hints {
    ($($result:ty),+ $(,)?) => {
        $(impl CacheHints for $result {
            fn with_cache_hints(self, ttl_ms: u64, scope: CacheScope) -> Self {
                self.with_ttl_ms(ttl_ms).with_cache_scope(scope)
            }
        })+
    };
}

impl_cache_hints!(
    rmcp::model::ListToolsResult,
    rmcp::model::ListResourcesResult,
    rmcp::model::ListPromptsResult,
    rmcp::model::ReadResourceResult,
);

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::absent(None, ProtocolVersion::V_2026_07_28, false)]
    #[case::legacy(
        Some(ProtocolVersion::V_2025_11_25),
        ProtocolVersion::V_2026_07_28,
        false
    )]
    #[case::supported(
        Some(ProtocolVersion::V_2026_07_28),
        ProtocolVersion::V_2026_07_28,
        true
    )]
    #[case::above_cap(
        Some(ProtocolVersion::V_2026_07_28),
        ProtocolVersion::V_2025_11_25,
        false
    )]
    fn respects_peer_and_server_versions(
        #[case] peer: Option<ProtocolVersion>,
        #[case] server_max: ProtocolVersion,
        #[case] expected: bool,
    ) {
        assert_eq!(supports_cache_hints(peer.as_ref(), &server_max), expected);
    }

    #[rstest]
    #[case::default(Caching::default().ttl_ms)]
    #[case::custom(60_000)]
    #[case::zero(0)]
    fn builders_preserve_response_fields(#[case] ttl_ms: u64) {
        fn check<T>(result: T, ttl_ms: u64)
        where
            T: CacheHints + serde::Serialize,
        {
            let mut expected = serde_json::to_value(&result).unwrap();
            expected["ttlMs"] = ttl_ms.into();
            expected["cacheScope"] = "private".into();
            let actual = result.with_cache_hints(ttl_ms, CacheScope::Private);
            assert_eq!(serde_json::to_value(actual).unwrap(), expected);
        }

        use rmcp::model::*;
        let mut tools = ListToolsResult::with_all_items(vec![Tool::new(
            "test",
            "description",
            serde_json::Map::new(),
        )]);
        tools.meta = Some(serde_json::from_value(serde_json::json!({"test": true})).unwrap());
        tools.next_cursor = Some("next".into());
        check(tools, ttl_ms);
        check(
            ListResourcesResult::with_all_items(vec![Resource::new("ui://test", "test")]),
            ttl_ms,
        );
        check(
            ListPromptsResult::with_all_items(vec![Prompt::new("test", Some("description"), None)]),
            ttl_ms,
        );
        check(
            ReadResourceResult::new(vec![ResourceContents::text("content", "ui://test")]),
            ttl_ms,
        );
    }
}
