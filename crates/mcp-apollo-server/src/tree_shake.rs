//! Tree shaking for GraphQL schema types

use crate::sanitize::Sanitize;
use apollo_compiler::ast::{FragmentDefinition, Selection};
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::{ExtendedType, ObjectType};
use apollo_compiler::{Name, Node, Schema};
use derive_new::new;

/// Tree shaker for GraphQL schema types
#[derive(new)]
pub struct TreeShaker<'schema> {
    schema: &'schema Schema,
    fragments: &'schema [&'schema Node<FragmentDefinition>],
    #[new(default)]
    types: IndexMap<Name, Vec<Selection>>,
}

impl<'schema> TreeShaker<'schema> {
    /// Retain the given type and anything reachable from it, filtering for the selection set.
    /// If the same type would be retained with multiple selection sets, a union of the selection
    /// sets is retained.
    pub fn retain(
        &mut self,
        type_name: Name,
        extended_type: &'schema ExtendedType,
        selection_set: &'schema [Selection],
    ) {
        let mut stack = vec![];
        self.types.insert(type_name.clone(), selection_set.to_vec());
        stack.push((type_name, extended_type.clone(), selection_set.to_vec()));
        while let Some((type_name, extended_type, selection_set)) = stack.pop() {
            self.inner_retain(type_name, extended_type, selection_set, &mut stack);
        }
    }

    fn inner_retain(
        &mut self,
        type_name: Name,
        extended_type: ExtendedType,
        selection_set: Vec<Selection>,
        stack: &mut Vec<(Name, ExtendedType, Vec<Selection>)>,
    ) {
        if let Some(existing) = self.types.get_mut(&type_name) {
            existing.extend(selection_set.clone());
        } else {
            self.types.insert(type_name, selection_set.clone());
        }
        match extended_type {
            ExtendedType::Object(ty) => {
                for (name, field) in ty.fields.iter() {
                    if let Some(sub_selection) = self.selected(name, &selection_set) {
                        let field_type_name = field.ty.inner_named_type().clone();
                        if let Some(field_type) =
                            self.schema.types.get(field.ty.inner_named_type().as_str())
                        {
                            stack.push((
                                field_type_name,
                                field_type.clone(),
                                sub_selection.clone(),
                            ));
                        }
                    }
                }
            }
            ExtendedType::Union(union) => {
                for member_type_name in union.members.iter() {
                    if let Some(member_type) = self.schema.types.get(member_type_name.name.as_str())
                    {
                        stack.push((
                            member_type_name.name.clone(),
                            member_type.clone(),
                            selection_set.to_vec(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    /// Determine if a field is selected by a selection set, and if so, return the sub-selection.
    fn selected(&self, field_name: &Name, selection: &[Selection]) -> Option<Vec<Selection>> {
        selection.iter().find_map(|selection| match selection {
            Selection::Field(field) => {
                if field.name.to_string() == field_name.to_string() {
                    Some(field.selection_set.clone())
                } else {
                    None
                }
            }
            Selection::InlineFragment(fragment) => {
                self.selected(field_name, &fragment.selection_set)
            }
            Selection::FragmentSpread(fragment) => self
                .fragments
                .iter()
                .find(|f| f.name.as_str() == fragment.fragment_name.as_str())
                .and_then(|f| self.selected(field_name, &f.selection_set)),
        })
    }

    /// Return the set of types retained after tree shaking.
    pub fn shaken(&self) -> impl Iterator<Item = ExtendedType> {
        self.types.iter().filter_map(|(name, selection_set)| {
            if let Some(extended_type) = self.schema.types.get(name.as_str()) {
                if extended_type.is_built_in() {
                    None
                } else {
                    match extended_type {
                        ExtendedType::Object(object_type) => {
                            let mut fields = IndexMap::default();
                            for (name, field) in object_type.fields.iter() {
                                if self.selected(name, selection_set).is_some() {
                                    fields.insert(
                                        name.clone(),
                                        field.as_ref().clone().sanitize().into(),
                                    );
                                }
                            }
                            Some(ExtendedType::from(ObjectType {
                                fields,
                                ..object_type.as_ref().clone().sanitize()
                            }))
                        }
                        ExtendedType::Scalar(ty) => {
                            Some(ExtendedType::from(ty.as_ref().clone().sanitize()))
                        }
                        ExtendedType::Enum(ty) => {
                            Some(ExtendedType::from(ty.as_ref().clone().sanitize()))
                        }
                        _ => None,
                    }
                }
            } else {
                None
            }
        })
    }
}
