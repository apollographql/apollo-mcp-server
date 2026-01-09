#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub(crate) mod apps;
pub(crate) mod auth;
pub mod cors;
pub mod custom_scalar_map;
pub mod env_expansion;
pub mod errors;
pub(crate) mod event;
mod explorer;
mod graphql;
pub mod headers;
pub mod health;
mod introspection;
pub(crate) mod json_schema;
pub(crate) mod meter;
pub mod operations;
pub(crate) mod schema_tree_shake;
pub mod server;
pub(crate) mod telemetry_attributes;

/// These values are generated at build time by build.rs using telemetry.toml as input.
pub mod generated {
    pub mod telemetry {
        include!(concat!(env!("OUT_DIR"), "/telemetry_attributes.rs"));
    }
}
