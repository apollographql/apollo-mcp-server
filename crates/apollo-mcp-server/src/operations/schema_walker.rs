//! JSON Schema generation utilities
//!
//! The types in this module generate JSON schemas for GraphQL types by walking
//! the types recursively.

use apollo_compiler::{Schema as GraphQLSchema, ast::Type};
use schemars::Schema;
use serde_json::{Map, Value};

use crate::custom_scalar_map::CustomScalarMap;

mod name;
mod output;
mod r#type;

pub use output::selection_set_to_schema;

/// Convert a GraphQL type into a JSON Schema.
///
/// Note: This is recursive, which might cause a stack overflow if the type is
/// sufficiently nested / complex.
pub fn type_to_schema(
    r#type: &Type,
    schema: &GraphQLSchema,
    definitions: &mut Map<String, Value>,
    custom_scalar_map: Option<&CustomScalarMap>,
    description: Option<String>,
) -> Schema {
    r#type::Type {
        cache: definitions,
        custom_scalar_map,
        description: &description,
        schema,
        r#type,
    }
    .into()
}

/// Modifies a schema to include an optional description
fn with_desc(mut schema: Schema, description: &Option<String>) -> Schema {
    if let Some(desc) = description {
        schema
            .ensure_object()
            .entry("description")
            .or_insert(desc.clone().into());
    }

    schema
}
