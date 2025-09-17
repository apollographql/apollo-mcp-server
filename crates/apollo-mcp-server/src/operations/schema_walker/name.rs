use apollo_compiler::{Name as GraphQLName, Node, Schema as GraphQLSchema, schema::ExtendedType};
use schemars::{Schema as JSONSchema, json_schema};
use serde_json::{Map, Value};
use tracing::warn;

use crate::custom_scalar_map::CustomScalarMap;

use super::{r#type::Type, with_desc};

/// A GraphQL Named Walker
pub(super) struct Name<'a> {
    /// The definition cache which contains full schemas for nested types
    pub(super) cache: &'a mut Map<String, Value>,

    /// Custom scalar map for supplementing information from the GraphQL schema
    pub(super) custom_scalar_map: Option<&'a CustomScalarMap>,

    /// The optional description of the named type, from comments in the schema
    pub(super) description: &'a Option<String>,

    /// The actual named type to translate into a JSON schema
    pub(super) name: &'a GraphQLName,

    /// The original GraphQL schema with all type information
    pub(super) schema: &'a GraphQLSchema,
}

impl From<Name<'_>> for JSONSchema {
    fn from(
        Name {
            cache,
            custom_scalar_map,
            description,
            name,
            schema,
        }: Name,
    ) -> Self {
        let unknown_type = json_schema!({});

        let result = match name.as_str() {
            // Basic types map nicely
            "String" | "ID" => json_schema!({"type": "string"}),
            "Int" | "Float" => json_schema!({"type": "number"}),
            "Boolean" => json_schema!({"type": "boolean"}),

            // If we've already cached it, then return the reference immediately
            cached if cache.contains_key(cached) => {
                JSONSchema::new_ref(format!("#/definitions/{cached}"))
            }

            // Otherwise generate the dependent type
            other => match schema.types.get(other) {
                // Enums need to collect descriptions per field while also enumerating
                // all possible values
                Some(ExtendedType::Enum(r#enum)) => {
                    // Collect all fields such that each field is shown as
                    // <Description>: <Enum value>
                    let values = r#enum
                        .values
                        .iter()
                        .map(|(name, value)| {
                            format!(
                                "{}: {}",
                                name,
                                value
                                    .description
                                    .as_ref()
                                    .map(|d| d.to_string())
                                    .unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    // Consolidate all of the values such that we get a high-level
                    // description (from the schema) followed by its values
                    let description = format!(
                        "{}\n\nValues:\n{}",
                        r#enum
                            .description
                            .as_ref()
                            .map(Node::as_str)
                            .unwrap_or_default(),
                        values
                    );

                    cache.insert(
                        other.to_string(),
                        with_desc(json_schema!({
                                "type": "string",
                                "enum": r#enum.values.iter().map(|(_, value)| serde_json::json!(value.value)).collect::<Vec<_>>(),
                            }),
                            &Some(description),
                        ).into(),
                    );
                    JSONSchema::new_ref(format!("#/definitions/{other}"))
                }

                // Input types need to be traversed over their fields to ensure that they copy over
                // nested structure.
                Some(ExtendedType::InputObject(input)) => {
                    // Insert temporary value into map so any recursive references will not try to also create it.
                    cache.insert(other.to_string(), Default::default());

                    let mut input_schema = with_desc(
                        json_schema!({"type": "object", "properties": {}}),
                        &input.description.as_ref().map(Node::to_string),
                    );
                    for (name, field) in input.fields.iter() {
                        let field_description = field.description.as_ref().map(|n| n.to_string());
                        input_schema
                            .ensure_object()
                            .entry("properties")
                            .or_insert(Value::Object(Default::default()))
                            .as_object_mut()
                            .get_or_insert(&mut Map::default())
                            .insert(
                                name.to_string(),
                                JSONSchema::from(Type {
                                    cache,
                                    custom_scalar_map,
                                    description: &field_description,
                                    schema,
                                    r#type: &field.ty,
                                })
                                .into(),
                            );

                        // Mark any non-nullable fields as being required
                        if field.is_required() {
                            input_schema
                                .ensure_object()
                                .entry("required")
                                .or_insert(Value::Array(Default::default()))
                                .as_array_mut()
                                .get_or_insert(&mut Vec::default())
                                .push(name.to_string().into());
                        }
                    }

                    cache.insert(other.to_string(), input_schema.into());
                    JSONSchema::new_ref(format!("#/definitions/{other}"))
                }

                // Custom scalars need to be opaquely copied over as types with no further processing
                Some(ExtendedType::Scalar(scalar)) => {
                    // The default scalar description should always be from the scalar in the schema itself
                    let default_scalar_description =
                        scalar.description.as_ref().map(Node::to_string);

                    if let Some(custom_scalar_map) = custom_scalar_map {
                        if let Some(custom_scalar_schema_object) = custom_scalar_map.get(other) {
                            // The custom scalar schema might have an override for the description, so we extract it here.
                            let mut scalar_schema = custom_scalar_schema_object.clone();
                            let description = scalar_schema
                                .ensure_object()
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string);

                            cache.insert(
                                other.to_string(),
                                with_desc(
                                    custom_scalar_schema_object.clone(),
                                    // The description could have been overridden by the custom schema, so we prioritize it here
                                    &description.or(default_scalar_description),
                                )
                                .into(),
                            );
                        } else {
                            warn!(name=?other, "custom scalar missing from custom_scalar_map");
                            cache.insert(
                                other.to_string(),
                                with_desc(JSONSchema::default(), &default_scalar_description)
                                    .into(),
                            );
                        }
                    } else {
                        warn!(name=?other, "custom scalars aren't currently supported without a custom_scalar_map");
                        cache.insert(
                            other.to_string(),
                            with_desc(JSONSchema::default(), &default_scalar_description).into(),
                        );
                    }

                    JSONSchema::new_ref(format!("#/definitions/{other}"))
                }

                // Anything else is unhandled
                _ => {
                    warn!(name=?other, "Type not found in schema");
                    unknown_type
                }
            },
        };

        with_desc(result, description)
    }
}
