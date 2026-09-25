//! JSON Schema generation for GraphQL output types (selection sets)
//!
//! This module generates JSON schemas from GraphQL operation selection sets,
//! enabling MCP tools to declare their output schema.

use std::{
    cell::OnceCell,
    collections::{BTreeSet, HashMap, HashSet},
    rc::Rc,
};

use apollo_compiler::{
    Name as GraphQLName, Node, Schema as GraphQLSchema,
    ast::{Field, Selection, Type as GraphQLType},
    collections::{HashMap as CompilerHashMap, IndexMap as CompilerIndexMap},
    schema::{ExtendedType, Implementers},
};
use schemars::{Schema as JSONSchema, json_schema};
use serde_json::{Map, Value};
use tracing::warn;

use crate::custom_scalar_map::CustomScalarMap;
use crate::operations::private_fields::PrivateFieldTree;

/// Generate a JSON Schema for the output of a GraphQL operation.
///
/// This walks the selection set and generates a schema that describes
/// the expected response structure.
pub fn selection_set_to_schema(
    selection_set: &[Selection],
    parent_type: &ExtendedType,
    graphql_schema: &GraphQLSchema,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    private_tree: Option<&PrivateFieldTree>,
) -> JSONSchema {
    let mut definitions = Map::new();
    let implementers = OnceCell::new();

    let schema = build_selection_set_schema(
        selection_set,
        parent_type,
        graphql_schema,
        &implementers,
        custom_scalar_map,
        named_fragments,
        &mut definitions,
        private_tree.unwrap_or(&PrivateFieldTree::default()),
    );

    // Wrap in standard GraphQL response envelope
    let mut response_schema = json_schema!({
        "type": "object",
        "properties": {
            "data": schema,
            "errors": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "locations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "line": { "type": "integer" },
                                    "column": { "type": "integer" }
                                }
                            }
                        },
                        "path": {
                            "type": "array",
                            "items": {
                                "oneOf": [
                                    { "type": "string" },
                                    { "type": "integer" }
                                ]
                            }
                        },
                        "extensions": { "type": "object" }
                    },
                    "required": ["message"]
                }
            },
            "extensions": { "type": "object" }
        }
    });

    // Add definitions if we collected any
    if !definitions.is_empty() {
        response_schema
            .ensure_object()
            .insert("definitions".to_string(), definitions.into());
    }

    response_schema
}

/// A field and the concrete members on which its enclosing fragments apply.
struct SelectedField<'a> {
    field: &'a Node<Field>,
    declared_on: &'a ExtendedType,
    members: Rc<[usize]>,
}

/// Expand the fragment graph once per selection set. The visited key includes the
/// applicable members because the same named fragment can occur under different
/// type conditions. Validated GraphQL documents do not contain fragment cycles.
#[allow(clippy::too_many_arguments)]
fn collect_selected_fields<'a>(
    selection_set: &'a [Selection],
    declared_on: &'a ExtendedType,
    member_names: &[&str],
    member_index: &HashMap<&str, usize>,
    applicable: Rc<[usize]>,
    graphql_schema: &'a GraphQLSchema,
    named_fragments: &'a HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    visited: &mut HashSet<(String, Vec<usize>)>,
    fields: &mut Vec<SelectedField<'a>>,
) {
    for selection in selection_set {
        match selection {
            Selection::Field(field) => fields.push(SelectedField {
                field,
                declared_on,
                members: applicable.clone(),
            }),
            Selection::InlineFragment(fragment) => {
                let target = fragment
                    .type_condition
                    .as_ref()
                    .and_then(|name| graphql_schema.types.get(name.as_str()));
                if fragment.type_condition.is_some() && target.is_none() {
                    continue;
                }
                let next = target.unwrap_or(declared_on);
                let matching = matching_members(
                    &applicable,
                    member_names,
                    member_index,
                    next,
                    graphql_schema,
                );
                if !matching.is_empty() {
                    collect_selected_fields(
                        &fragment.selection_set,
                        next,
                        member_names,
                        member_index,
                        Rc::from(matching),
                        graphql_schema,
                        named_fragments,
                        visited,
                        fields,
                    );
                }
            }
            Selection::FragmentSpread(spread) => {
                if let Some(fragment) = named_fragments.get(spread.fragment_name.as_str())
                    && let Some(next) = graphql_schema.types.get(fragment.type_condition.as_str())
                {
                    let matching = matching_members(
                        &applicable,
                        member_names,
                        member_index,
                        next,
                        graphql_schema,
                    );
                    if !matching.is_empty()
                        && visited.insert((spread.fragment_name.to_string(), matching.clone()))
                    {
                        collect_selected_fields(
                            &fragment.selection_set,
                            next,
                            member_names,
                            member_index,
                            Rc::from(matching),
                            graphql_schema,
                            named_fragments,
                            visited,
                            fields,
                        );
                    }
                }
            }
        }
    }
}

fn matching_members(
    applicable: &[usize],
    member_names: &[&str],
    member_index: &HashMap<&str, usize>,
    condition: &ExtendedType,
    graphql_schema: &GraphQLSchema,
) -> Vec<usize> {
    if matches!(condition, ExtendedType::Object(_)) {
        return member_index
            .get(condition.name().as_str())
            .copied()
            .filter(|index| applicable.binary_search(index).is_ok())
            .into_iter()
            .collect();
    }
    // `applicable` starts in member order; filtering preserves the sorted indices
    // used by binary_search when grouping selection patterns.
    applicable
        .iter()
        .copied()
        .filter(|&index| {
            member_names.get(index).is_some_and(|member| {
                condition.name().as_str() == *member
                    || graphql_schema.is_subtype(condition.name().as_str(), member)
            })
        })
        .collect()
}

/// Build one schema per distinct fragment applicability pattern, not per member.
#[allow(clippy::too_many_arguments)]
fn build_selection_set_schema(
    selection_set: &[Selection],
    parent_type: &ExtendedType,
    graphql_schema: &GraphQLSchema,
    implementers: &OnceCell<CompilerHashMap<GraphQLName, Implementers>>,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
    private_tree: &PrivateFieldTree,
) -> JSONSchema {
    let member_names: Vec<&str> = match parent_type {
        ExtendedType::Union(union) => union.members.iter().map(|m| m.as_str()).collect(),
        ExtendedType::Interface(interface) => implementers
            .get_or_init(|| graphql_schema.implementers_map())
            .get(&interface.name)
            .map(|types| types.objects.iter().map(|m| m.as_str()).collect())
            .unwrap_or_default(),
        _ => vec![parent_type.name().as_str()],
    };
    // An interface with no implementers has no concrete response; retain a
    // useful schema for its declared fields without emitting an empty anyOf.
    let member_names = if member_names.is_empty() {
        vec![parent_type.name().as_str()]
    } else {
        member_names
    };
    let all_members: Vec<usize> = (0..member_names.len()).collect();
    let member_index: HashMap<_, _> = member_names
        .iter()
        .enumerate()
        .map(|(index, name)| (*name, index))
        .collect();
    let mut fields = Vec::new();
    collect_selected_fields(
        selection_set,
        parent_type,
        &member_names,
        &member_index,
        Rc::from(all_members),
        graphql_schema,
        named_fragments,
        &mut HashSet::new(),
        &mut fields,
    );

    let common: Vec<_> = fields
        .iter()
        .filter(|field| field.members.len() == member_names.len())
        .collect();
    let common_schema = build_fields_schema(
        &common,
        graphql_schema,
        implementers,
        custom_scalar_map,
        named_fragments,
        definitions,
        private_tree,
    );
    let conditional: Vec<_> = fields
        .iter()
        .filter(|field| field.members.len() != member_names.len())
        .collect();
    if conditional.is_empty() {
        return common_schema;
    }

    let mut signatures = vec![Vec::new(); member_names.len()];
    for (field_index, field) in conditional.iter().enumerate() {
        for &member in field.members.iter() {
            if let Some(signature) = signatures.get_mut(member) {
                signature.push(field_index);
            }
        }
    }
    let groups: BTreeSet<_> = signatures.into_iter().collect();
    let common_keys: BTreeSet<_> = common
        .iter()
        .map(|field| response_key(field.field))
        .collect();
    let fallback_exclusions: BTreeSet<_> = conditional
        .iter()
        .map(|field| response_key(field.field))
        .filter(|key| {
            !common_keys.contains(key)
                && !private_tree
                    .children
                    .get(key)
                    .is_some_and(|child| child.is_private)
        })
        .collect();
    let group_keys: Vec<BTreeSet<_>> = groups
        .iter()
        .map(|signature| {
            signature
                .iter()
                .filter_map(|&index| conditional.get(index))
                .map(|field| response_key(field.field))
                .filter(|key| fallback_exclusions.contains(key))
                .collect()
        })
        .collect();
    let other_keys = if fallback_exclusions.is_empty() {
        None
    } else {
        // Share the set of conditional keys across branches. Each branch may
        // contain only its own conditional keys, but remains open to unrelated
        // response keys just like an ordinary object schema.
        let name = format!("$otherResponseKeys{}", definitions.len());
        definitions.insert(
            name.clone(),
            json_schema!({"not": {"enum": fallback_exclusions}}).into(),
        );
        Some(JSONSchema::new_ref(format!("#/definitions/{name}")))
    };
    let mut group_counts = vec![0; conditional.len()];
    for signature in &groups {
        for &index in signature {
            if let Some(count) = group_counts.get_mut(index) {
                *count += 1;
            }
        }
    }
    let mut shared_fields = HashMap::new();
    for (index, field) in conditional.iter().enumerate() {
        if group_counts.get(index).copied().unwrap_or_default() < 2
            || private_tree
                .children
                .get(&response_key(field.field))
                .is_some_and(|child| child.is_private)
        {
            continue;
        }
        // A nested field may apply to several member patterns. Build its schema
        // once so its subtree does not multiply at each level of nesting.
        let schema = build_fields_schema(
            std::slice::from_ref(field),
            graphql_schema,
            implementers,
            custom_scalar_map,
            named_fragments,
            definitions,
            private_tree,
        );
        let name = format!("$sharedField{}", definitions.len());
        definitions.insert(name.clone(), schema.into());
        shared_fields.insert(index, JSONSchema::new_ref(format!("#/definitions/{name}")));
    }
    let mut branches = Vec::new();
    for (signature, keys) in groups.into_iter().zip(&group_keys) {
        let selected: Vec<_> = signature
            .iter()
            .filter_map(|&index| {
                if shared_fields.contains_key(&index) {
                    None
                } else {
                    conditional.get(index).copied()
                }
            })
            .collect();
        let mut branch = build_fields_schema(
            &selected,
            graphql_schema,
            implementers,
            custom_scalar_map,
            named_fragments,
            definitions,
            private_tree,
        );
        let shared: Vec<_> = signature
            .iter()
            .filter_map(|index| shared_fields.get(index).cloned())
            .collect();
        if !shared.is_empty() {
            let mut constraints = Vec::with_capacity(shared.len() + 1);
            constraints.push(branch);
            constraints.extend(shared);
            branch = json_schema!({"allOf": constraints});
        }
        if let Some(other_keys) = &other_keys {
            let allowed = if keys.is_empty() {
                other_keys.clone()
            } else {
                json_schema!({"anyOf": [{"enum": keys}, other_keys]})
            };
            branch
                .ensure_object()
                .insert("propertyNames".into(), allowed.into());
        }
        branches.push(branch);
    }
    let alternatives = json_schema!({"anyOf": branches});
    json_schema!({"allOf": [common_schema, alternatives]})
}

fn response_key(field: &Field) -> String {
    field.alias.as_ref().unwrap_or(&field.name).to_string()
}

#[allow(clippy::too_many_arguments)]
fn build_fields_schema(
    selected: &[&SelectedField<'_>],
    graphql_schema: &GraphQLSchema,
    implementers: &OnceCell<CompilerHashMap<GraphQLName, Implementers>>,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
    private_tree: &PrivateFieldTree,
) -> JSONSchema {
    let mut field_schemas: CompilerIndexMap<String, Vec<Value>> = CompilerIndexMap::default();
    let mut required = Vec::new();
    let mut required_seen = HashSet::new();
    for selected_field in selected {
        let field = selected_field.field;
        let key = response_key(field);
        if private_tree
            .children
            .get(&key)
            .is_some_and(|child| child.is_private)
        {
            continue;
        }
        let schema = if field.name.as_str() == "__typename" {
            json_schema!({"type": "string", "description": "The typename of this object"})
        } else if let Some(definition) =
            get_field_definition(selected_field.declared_on, field.name.as_str())
        {
            if definition.ty.is_non_null() && required_seen.insert(key.clone()) {
                required.push(key.clone());
            }
            let child_private_tree = private_tree.children.get(&key).cloned().unwrap_or_default();
            build_field_schema(
                field,
                &definition.ty,
                graphql_schema,
                implementers,
                custom_scalar_map,
                named_fragments,
                definitions,
                definition.description.as_ref().map(ToString::to_string),
                &child_private_tree,
            )
        } else {
            warn!(field = %field.name, parent_type = %selected_field.declared_on.name(), "Field not found in parent type");
            continue;
        };
        let value: Value = schema.into();
        let schemas = field_schemas.entry(key).or_default();
        if schemas.last() != Some(&value) {
            schemas.push(value);
        }
    }
    let properties: Map<_, _> = field_schemas
        .into_iter()
        .map(|(key, mut schemas)| {
            let schema = if schemas.len() == 1 {
                schemas.remove(0)
            } else {
                json_schema!({"allOf": schemas}).into()
            };
            (key, schema)
        })
        .collect();
    let mut schema = json_schema!({"type": "object"});
    if !properties.is_empty() {
        schema
            .ensure_object()
            .insert("properties".into(), properties.into());
    }
    if !required.is_empty() {
        schema.ensure_object().insert(
            "required".into(),
            required
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>()
                .into(),
        );
    }
    schema
}

/// Build schema for a specific field based on its type
#[allow(clippy::too_many_arguments)]
fn build_field_schema(
    field: &Node<Field>,
    field_type: &GraphQLType,
    graphql_schema: &GraphQLSchema,
    implementers: &OnceCell<CompilerHashMap<GraphQLName, Implementers>>,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
    description: Option<String>,
    private_tree: &PrivateFieldTree,
) -> JSONSchema {
    let schema = type_to_output_schema(
        field_type,
        &field.selection_set,
        graphql_schema,
        implementers,
        custom_scalar_map,
        named_fragments,
        definitions,
        private_tree,
    );

    with_description(schema, description)
}

/// Convert a GraphQL type to a JSON Schema for output
#[allow(clippy::too_many_arguments)]
fn type_to_output_schema(
    graphql_type: &GraphQLType,
    selection_set: &[Selection],
    graphql_schema: &GraphQLSchema,
    implementers: &OnceCell<CompilerHashMap<GraphQLName, Implementers>>,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
    private_tree: &PrivateFieldTree,
) -> JSONSchema {
    match graphql_type {
        // Non-null types - just unwrap
        GraphQLType::NonNullNamed(name) => named_type_to_output_schema(
            name,
            selection_set,
            graphql_schema,
            implementers,
            custom_scalar_map,
            named_fragments,
            definitions,
            private_tree,
        ),
        GraphQLType::NonNullList(inner) => {
            let items = type_to_output_schema(
                inner.as_ref(),
                selection_set,
                graphql_schema,
                implementers,
                custom_scalar_map,
                named_fragments,
                definitions,
                private_tree,
            );
            json_schema!({
                "type": "array",
                "items": items
            })
        }

        // Nullable types - allow null. `anyOf` rather than `oneOf`: the inner schema may
        // itself accept `null`, and `oneOf` would then reject it for matching both branches.
        GraphQLType::Named(name) => {
            let inner = named_type_to_output_schema(
                name,
                selection_set,
                graphql_schema,
                implementers,
                custom_scalar_map,
                named_fragments,
                definitions,
                private_tree,
            );
            json_schema!({
                "anyOf": [inner, {"type": "null"}]
            })
        }
        GraphQLType::List(inner) => {
            let items = type_to_output_schema(
                inner.as_ref(),
                selection_set,
                graphql_schema,
                implementers,
                custom_scalar_map,
                named_fragments,
                definitions,
                private_tree,
            );
            json_schema!({
                "anyOf": [
                    {"type": "array", "items": items},
                    {"type": "null"}
                ]
            })
        }
    }
}

/// Convert a named GraphQL type to JSON Schema
#[allow(clippy::too_many_arguments)]
fn named_type_to_output_schema(
    name: &GraphQLName,
    selection_set: &[Selection],
    graphql_schema: &GraphQLSchema,
    implementers: &OnceCell<CompilerHashMap<GraphQLName, Implementers>>,
    custom_scalar_map: Option<&CustomScalarMap>,
    named_fragments: &HashMap<String, Node<apollo_compiler::ast::FragmentDefinition>>,
    definitions: &mut Map<String, Value>,
    private_tree: &PrivateFieldTree,
) -> JSONSchema {
    match name.as_str() {
        // Built-in scalars
        "String" => json_schema!({"type": "string"}),
        "Int" => json_schema!({"type": "integer"}),
        "Float" => json_schema!({"type": "number"}),
        "Boolean" => json_schema!({"type": "boolean"}),
        // ID can be serialized as string or integer depending on the GraphQL server
        "ID" => json_schema!({"oneOf": [{"type": "string"}, {"type": "integer"}]}),

        // Check cache first
        other if definitions.contains_key(other) => {
            JSONSchema::new_ref(format!("#/definitions/{other}"))
        }

        // Look up in schema
        other => match graphql_schema.types.get(other) {
            // Object types - recurse into selection set
            Some(ExtendedType::Object(obj)) => {
                if selection_set.is_empty() {
                    // No selection set - just reference the type
                    warn!(
                        type_name = other,
                        "Object type without selection set in output schema"
                    );
                    json_schema!({})
                } else {
                    build_selection_set_schema(
                        selection_set,
                        &ExtendedType::Object(obj.clone()),
                        graphql_schema,
                        implementers,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                        private_tree,
                    )
                }
            }

            // Interface types - similar to objects
            Some(ExtendedType::Interface(iface)) => {
                if selection_set.is_empty() {
                    json_schema!({})
                } else {
                    build_selection_set_schema(
                        selection_set,
                        &ExtendedType::Interface(iface.clone()),
                        graphql_schema,
                        implementers,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                        private_tree,
                    )
                }
            }

            // Resolve fragment applicability for each concrete union member.
            Some(ExtendedType::Union(union)) => {
                if selection_set.is_empty() {
                    json_schema!({})
                } else {
                    build_selection_set_schema(
                        selection_set,
                        &ExtendedType::Union(union.clone()),
                        graphql_schema,
                        implementers,
                        custom_scalar_map,
                        named_fragments,
                        definitions,
                        private_tree,
                    )
                }
            }

            // Enum types
            // Note: We only include the enum's type description (not per-value descriptions)
            // to avoid token bloat with large enums. The `enum` constraint already lists
            // all valid values, which is sufficient for understanding output.
            Some(ExtendedType::Enum(enum_def)) => {
                let values: Vec<Value> = enum_def
                    .values
                    .iter()
                    .map(|(_, v)| serde_json::json!(v.value))
                    .collect();

                let mut enum_schema = json_schema!({
                    "type": "string",
                    "enum": values
                });

                // Only include the enum's type description, not per-value descriptions
                if let Some(desc) = &enum_def.description {
                    enum_schema
                        .ensure_object()
                        .insert("description".to_string(), desc.to_string().into());
                }

                definitions.insert(other.to_string(), enum_schema.into());

                JSONSchema::new_ref(format!("#/definitions/{other}"))
            }

            // Custom scalars
            Some(ExtendedType::Scalar(scalar)) => {
                let description = scalar.description.as_ref().map(|n| n.to_string());

                if let Some(custom_map) = custom_scalar_map
                    && let Some(custom_schema) = custom_map.get(other)
                {
                    return with_description(custom_schema.clone(), description);
                }

                // Unknown scalar - return empty schema with description
                with_description(json_schema!({}), description)
            }

            // InputObject shouldn't appear in output, but handle gracefully
            Some(ExtendedType::InputObject(_)) => {
                warn!(
                    type_name = other,
                    "InputObject type found in output schema - this is unexpected"
                );
                json_schema!({})
            }

            None => {
                warn!(type_name = other, "Type not found in schema");
                json_schema!({})
            }
        },
    }
}

/// Get field definition from a parent type (Object or Interface)
fn get_field_definition(
    parent_type: &ExtendedType,
    field_name: &str,
) -> Option<Node<apollo_compiler::schema::FieldDefinition>> {
    match parent_type {
        ExtendedType::Object(obj) => obj.fields.get(field_name).map(|f| f.node.clone()),
        ExtendedType::Interface(iface) => iface.fields.get(field_name).map(|f| f.node.clone()),
        _ => None,
    }
}

/// Add description to a schema if provided
fn with_description(mut schema: JSONSchema, description: Option<String>) -> JSONSchema {
    if let Some(desc) = description {
        schema
            .ensure_object()
            .entry("description")
            .or_insert(desc.into());
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_compiler::parser::Parser;
    use rstest::rstest;

    use crate::operations::private_fields::{collect_named_fragments, collect_private_fields};

    fn parse_schema(sdl: &str) -> GraphQLSchema {
        GraphQLSchema::parse_and_validate(sdl, "schema.graphql")
            .unwrap()
            .into_inner()
    }

    fn parse_operation(query: &str) -> (apollo_compiler::ast::Document, Vec<Selection>) {
        let doc = Parser::new().parse_ast(query, "query.graphql").unwrap();
        let selection_set = doc
            .definitions
            .iter()
            .find_map(|def| match def {
                apollo_compiler::ast::Definition::OperationDefinition(op) => {
                    Some(op.selection_set.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        (doc, selection_set)
    }

    #[test]
    fn simple_query_output_schema() {
        let schema = parse_schema(
            r#"
            type Query {
                "Get a user by ID"
                user(id: ID!): User
            }

            "A user in the system"
            type User {
                "The user's unique identifier"
                id: ID!
                "The user's display name"
                name: String!
                "The user's email address"
                email: String
            }
            "#,
        );

        let (_, selection_set) = parse_operation(
            r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    name
                    email
                }
            }
            "#,
        );

        let query_type = schema.types.get("Query").unwrap();
        let output_schema = selection_set_to_schema(
            &selection_set,
            query_type,
            &schema,
            None,
            &HashMap::new(),
            None,
        );

        insta::assert_snapshot!(serde_json::to_string_pretty(&output_schema).unwrap());
    }

    #[test]
    fn nested_object_output_schema() {
        let schema = parse_schema(
            r#"
            type Query {
                user(id: ID!): User
            }

            type User {
                id: ID!
                profile: Profile!
            }

            type Profile {
                bio: String
                avatar: String!
            }
            "#,
        );

        let (_, selection_set) = parse_operation(
            r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    profile {
                        bio
                        avatar
                    }
                }
            }
            "#,
        );

        let query_type = schema.types.get("Query").unwrap();
        let output_schema = selection_set_to_schema(
            &selection_set,
            query_type,
            &schema,
            None,
            &HashMap::new(),
            None,
        );

        insta::assert_snapshot!(serde_json::to_string_pretty(&output_schema).unwrap());
    }

    #[rstest]
    #[case::null_scalar(serde_json::json!({"id": "1", "meta": null, "tags": []}))]
    #[case::null_list(serde_json::json!({"id": "1", "meta": {"k": "v"}, "tags": null}))]
    #[case::null_list_item(serde_json::json!({"id": "1", "meta": {"k": "v"}, "tags": [null, 1]}))]
    fn output_schema_accepts_null_for_unmapped_custom_scalar(#[case] thing: Value) {
        let schema = parse_schema(
            r#"
            scalar JSON

            type Thing {
                id: ID!
                meta: JSON
                tags: [JSON]
            }

            type Query {
                thing: Thing
            }
            "#,
        );

        let (_, selection_set) = parse_operation(
            r#"
            query GetThing {
                thing {
                    id
                    meta
                    tags
                }
            }
            "#,
        );

        let query_type = schema.types.get("Query").unwrap();
        let output_schema = selection_set_to_schema(
            &selection_set,
            query_type,
            &schema,
            None,
            &HashMap::new(),
            None,
        );

        let validator = jsonschema::validator_for(&serde_json::to_value(&output_schema).unwrap())
            .expect("emitted output schema should itself be a valid JSON Schema");
        let response = serde_json::json!({"data": {"thing": thing}});

        let errors: Vec<String> = validator
            .iter_errors(&response)
            .map(|e| format!("{}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "response must validate against the emitted output schema, got: {errors:#?}"
        );
    }

    #[test]
    fn output_schema_accepts_union_member_matching_several_fragments() {
        let schema = parse_schema(
            r#"
            type Book {
                id: ID!
                name: String
            }

            type Author {
                id: ID!
                name: String
            }

            union SearchResult = Book | Author

            type Query {
                search: SearchResult!
            }
            "#,
        );

        let (_, selection_set) = parse_operation(
            r#"
            query Search {
                search {
                    ... on Book { id name }
                    ... on Author { id name }
                }
            }
            "#,
        );

        let query_type = schema.types.get("Query").unwrap();
        let output_schema = selection_set_to_schema(
            &selection_set,
            query_type,
            &schema,
            None,
            &HashMap::new(),
            None,
        );

        let validator = jsonschema::validator_for(&serde_json::to_value(&output_schema).unwrap())
            .expect("emitted output schema should itself be a valid JSON Schema");
        let response = serde_json::json!({"data": {"search": {"id": "1", "name": "Dune"}}});

        let errors: Vec<String> = validator
            .iter_errors(&response)
            .map(|e| format!("{}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "a member object that matches more than one fragment must validate, got: {errors:#?}"
        );
    }

    #[test]
    fn private_fields_excluded_from_output_schema() {
        let schema = parse_schema(
            r#"
            type Query {
                user(id: ID!): User
            }

            type User {
                id: ID!
                name: String!
                email: String
                secret: String
            }
            "#,
        );

        let (doc, selection_set) = parse_operation(
            r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    name
                    email @private
                    secret @private
                }
            }
            "#,
        );

        let named_fragments = collect_named_fragments(&doc);
        let private_tree = collect_private_fields(&selection_set, &named_fragments);

        let query_type = schema.types.get("Query").unwrap();
        let output_schema = selection_set_to_schema(
            &selection_set,
            query_type,
            &schema,
            None,
            &HashMap::new(),
            Some(&private_tree),
        );

        insta::assert_snapshot!(serde_json::to_string_pretty(&output_schema).unwrap());
    }

    #[test]
    fn private_field_in_operation_excluded_when_fragment_spread_reintroduces_it() {
        let schema = parse_schema(
            r#"
            type Query {
                user(id: ID!): User
            }

            type User {
                id: ID!
                name: String!
                email: String
            }
            "#,
        );

        let query = r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    name
                    email @private
                    ...UserFields
                }
            }
            fragment UserFields on User {
                email
            }
        "#;

        let (doc, selection_set) = parse_operation(query);
        let named_fragments = collect_named_fragments(&doc);
        let private_tree = collect_private_fields(&selection_set, &named_fragments);

        let query_type = schema.types.get("Query").unwrap();
        let output_schema = selection_set_to_schema(
            &selection_set,
            query_type,
            &schema,
            None,
            &named_fragments,
            Some(&private_tree),
        );

        let output_str = serde_json::to_string_pretty(&output_schema).unwrap();
        assert!(
            !output_str.contains("email"),
            "email should be excluded from schema because it is marked @private, but got: {output_str}"
        );
    }

    #[test]
    fn private_field_in_operation_excluded_when_inline_fragment_reintroduces_it() {
        let schema = parse_schema(
            r#"
            type Query {
                user(id: ID!): User
            }

            type User {
                id: ID!
                name: String!
                email: String
            }
            "#,
        );

        let query = r#"
            query GetUser($id: ID!) {
                user(id: $id) {
                    id
                    name
                    email @private
                    ... on User {
                        email
                    }
                }
            }
        "#;

        let (doc, selection_set) = parse_operation(query);
        let named_fragments = collect_named_fragments(&doc);
        let private_tree = collect_private_fields(&selection_set, &named_fragments);

        let query_type = schema.types.get("Query").unwrap();
        let output_schema = selection_set_to_schema(
            &selection_set,
            query_type,
            &schema,
            None,
            &HashMap::new(),
            Some(&private_tree),
        );

        let output_str = serde_json::to_string_pretty(&output_schema).unwrap();
        assert!(
            !output_str.contains("email"),
            "email should be excluded from schema because it is marked @private, but got: {output_str}"
        );
    }
}
