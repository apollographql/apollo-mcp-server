#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod auth;
pub mod custom_scalar_map;
pub mod errors;
pub mod event;
mod explorer;
mod graphql;
pub mod health;
mod introspection;
pub mod json_schema;
pub(crate) mod meter;
pub mod operations;
pub mod sanitize;
pub(crate) mod schema_tree_shake;
pub mod server;
pub mod telemetry_attributes;

pub mod generated {
    pub mod telemetry {
        include!(concat!(env!("OUT_DIR"), "/telemetry_attributes.rs"));
    }
}
