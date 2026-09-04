use apollo_compiler::{Schema as GraphQLSchema, ast::Type as GraphQLType};
use schemars::{Schema as JSONSchema, json_schema};
use serde_json::{Map, Value};

use crate::custom_scalar_map::CustomScalarMap;

use super::{name::Name, with_desc};

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
        let inner = match r#type {
            GraphQLType::List(items) | GraphQLType::NonNullList(items) => {
                let items: JSONSchema = Type {
                    cache,
                    custom_scalar_map,
                    description: &None,
                    schema,
                    r#type: items,
                }
                .into();

                json_schema!({
                    "type": "array",
                    "items": items,
                })
            }

            GraphQLType::Named(name) | GraphQLType::NonNullNamed(name) => JSONSchema::from(Name {
                cache,
                custom_scalar_map,
                name,
                schema,
            }),
        };

        // Spell out nullability instead of implying it by absence from `required`, so
        // clients that move every property into `required` can still pass null.
        // `anyOf` rather than `oneOf`: the strict-mode schema subsets of both OpenAI
        // and Anthropic accept `anyOf` and reject `oneOf`.
        let nullable = if r#type.is_non_null() {
            inner
        } else {
            json_schema!({
                "anyOf": [inner, {"type": "null"}],
            })
        };

        with_desc(nullable, description)
    }
}
