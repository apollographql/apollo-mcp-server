//! Provides an extension trait for traversing GraphQL schemas, using a depth-first traversal
//! starting at the specified root operation types (query, mutation, subscription).

use crate::OperationType;
use crate::path::PathNode;
use apollo_compiler::Schema;
use apollo_compiler::ast::NamedType;
use apollo_compiler::schema::ExtendedType;
use enumset::EnumSet;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

/// Extension trait to allow traversing a schema
pub trait SchemaExt {
    /// Traverse the type hierarchy in the schema in depth-first order, starting with the specified
    /// root operation types
    fn traverse(
        &self,
        root_types: EnumSet<OperationType>,
    ) -> Box<dyn Iterator<Item = (&ExtendedType, PathNode)> + '_>;
}

impl SchemaExt for Schema {
    fn traverse(
        &self,
        root_types: EnumSet<OperationType>,
    ) -> Box<dyn Iterator<Item = (&ExtendedType, PathNode)> + '_> {
        let mut stack = vec![];
        let mut references: HashMap<&NamedType, Vec<NamedType>> = HashMap::default();
        for root_type in root_types
            .iter()
            .rev()
            .filter_map(|rt| self.root_operation(rt.into()))
        {
            stack.push((root_type, PathNode::new(root_type.clone())));
        }
        Box::new(std::iter::from_fn(move || {
            while let Some((named_type, current_path)) = stack.pop() {
                if current_path.has_cycle() {
                    continue;
                }
                let references = references.entry(named_type);

                // Only traverse the children of a type the first time we visit it.
                // After that, we still visit unique paths to the type, but not the child paths.
                let traverse_children: bool = matches!(references, Entry::Vacant(_));

                references.or_insert(
                    current_path
                        .referencing_type()
                        .map(|(t, _, _)| vec![t.clone()])
                        .unwrap_or_default(),
                );

                let cloned = current_path.clone();
                if let Some(extended_type) = self.types.get(named_type) {
                    if !extended_type.is_built_in() {
                        if traverse_children {
                            match extended_type {
                                ExtendedType::Object(obj) => {
                                    stack.extend(obj.fields.values().map(|field| {
                                        let field_type = field.ty.inner_named_type();
                                        let field_args = field
                                            .arguments
                                            .iter()
                                            .map(|arg| arg.ty.inner_named_type().clone())
                                            .collect::<Vec<_>>();
                                        (
                                            field_type,
                                            current_path.clone().add_child(
                                                Some(field.name.clone()),
                                                field_args,
                                                field_type.clone(),
                                            ),
                                        )
                                    }));
                                }
                                ExtendedType::Interface(interface) => {
                                    stack.extend(interface.fields.values().map(|field| {
                                        let field_type = field.ty.inner_named_type();
                                        let field_args = field
                                            .arguments
                                            .iter()
                                            .map(|arg| arg.ty.inner_named_type().clone())
                                            .collect::<Vec<_>>();
                                        (
                                            field_type,
                                            current_path.clone().add_child(
                                                Some(field.name.clone()),
                                                field_args,
                                                field_type.clone(),
                                            ),
                                        )
                                    }));
                                }
                                ExtendedType::InputObject(input) => {
                                    stack.extend(input.fields.values().map(|field| {
                                        let next_type = field.ty.inner_named_type();
                                        (
                                            next_type,
                                            current_path.clone().add_child(
                                                Some(field.name.clone()),
                                                vec![],
                                                next_type.clone(),
                                            ),
                                        )
                                    }));
                                }
                                ExtendedType::Union(union) => {
                                    stack.extend(
                                        union.members.iter().map(|member| &member.name).map(
                                            |next_type| {
                                                (
                                                    next_type,
                                                    current_path.clone().add_child(
                                                        None,
                                                        vec![],
                                                        next_type.clone(),
                                                    ),
                                                )
                                            },
                                        ),
                                    );
                                }
                                _ => {}
                            }
                        }
                        return Some((extended_type, cloned));
                    }
                }
            }
            None
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_compiler::validation::Valid;
    use rstest::{fixture, rstest};

    const TEST_SCHEMA: &str = include_str!("testdata/schema.graphql");

    #[fixture]
    fn schema() -> Valid<Schema> {
        Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse test schema")
            .validate()
            .expect("Failed to validate test schema")
    }

    #[rstest]
    fn test_schema_traverse(schema: Valid<Schema>) {
        let mut paths = vec![];
        for (_extended_type, path) in schema
            .traverse(OperationType::Query | OperationType::Mutation | OperationType::Subscription)
        {
            paths.push(path.to_string());
        }
        insta::assert_debug_snapshot!(paths);
    }
}
