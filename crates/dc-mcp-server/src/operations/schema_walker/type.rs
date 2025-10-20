use apollo_compiler::{Schema as GraphQLSchema, ast::Type as GraphQLType};
use schemars::{Schema as JSONSchema, json_schema};
use serde_json::{Map, Value};

use crate::custom_scalar_map::CustomScalarMap;

use super::name::Name;

pub(super) struct Type<'a> {
    /// The definition cache which contains full schemas for nested types
    pub(super) cache: &'a mut Map<String, Value>,

    /// Custom scalar map for supplementing information from the GraphQL schema
    pub(super) custom_scalar_map: Option<&'a CustomScalarMap>,

    /// The optional description of the type, from comments in the schema
    pub(super) description: &'a Option<String>,

    /// The original GraphQL schema with all type information
    pub(super) schema: &'a GraphQLSchema,

    /// The actual type to translate into a JSON schema
    pub(super) r#type: &'a GraphQLType,
}

impl From<Type<'_>> for JSONSchema {
    fn from(
        Type {
            cache,
            custom_scalar_map,
            description,
            schema,
            r#type,
        }: Type,
    ) -> Self {
        // JSON Schema assumes that all properties are nullable unless there is a
        // required field, so we treat cases the same here.
        match r#type {
            GraphQLType::List(list) | GraphQLType::NonNullList(list) => {
                let nested_schema: JSONSchema = Type {
                    cache,
                    custom_scalar_map,
                    description,
                    schema,
                    r#type: list,
                }
                .into();

                // Arrays, however, do need to specify that fields can be null
                let nested_schema = if list.is_non_null() {
                    nested_schema
                } else {
                    json_schema!({"oneOf": [
                        nested_schema,
                        {"type": "null"},
                    ]})
                };

                json_schema!({
                    "type": "array",
                    "items": nested_schema,
                })
            }

            GraphQLType::Named(name) | GraphQLType::NonNullNamed(name) => JSONSchema::from(Name {
                cache,
                custom_scalar_map,
                description,
                name,
                schema,
            }),
        }
    }
}
