#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub(crate) mod apps;
pub(crate) mod auth;
pub mod caching;
pub mod cors;
pub mod custom_scalar_map;
pub mod env_expansion;
pub mod errors;
pub(crate) mod event;
mod explorer;
mod graphql;
pub mod headers;
pub mod health;
pub mod host_validation;
mod introspection;
pub(crate) mod json_schema;
pub(crate) mod meter;
pub mod operations;
pub(crate) mod prompts;
pub(crate) mod schema_tree_shake;
pub mod scope_requirements;
pub mod server;
pub mod server_info;
pub(crate) mod telemetry_attributes;

/// These values are generated at build time by build.rs using telemetry.toml as input.
pub mod generated {
    pub mod telemetry {
        include!(concat!(env!("OUT_DIR"), "/telemetry_attributes.rs"));
    }
}

/// Serializes tests that replace a process-global OpenTelemetry provider.
///
/// `global::set_meter_provider` swaps process-wide state, and every unit test
/// in this crate shares one test binary running them concurrently. A test that
/// installs a provider and then reads what reached it must hold this across
/// both steps, or another test's swap lands in between and it observes an
/// empty export.
#[cfg(test)]
pub(crate) static GLOBAL_TELEMETRY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
