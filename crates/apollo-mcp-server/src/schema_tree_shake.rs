//! Tree shaking for GraphQL schemas

use apollo_compiler::ast::{
    Definition, DirectiveDefinition, DirectiveList, Document, EnumTypeDefinition, Field,
    FragmentDefinition, InputObjectTypeDefinition, InterfaceTypeDefinition, ObjectTypeDefinition,
    OperationDefinition, OperationType, ScalarTypeDefinition, SchemaDefinition, Selection,
    UnionTypeDefinition,
};
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::WithErrors;
use apollo_compiler::{Node, Schema};
use std::collections::HashMap;

struct RootOperationNames {
    query: String,
    mutation: String,
    subscription: String,
}
impl RootOperationNames {
    fn operation_name(
        operation_type: OperationType,
        default_name: &str,
        schema: &Schema,
    ) -> String {
        schema
            .root_operation(operation_type)
            .map(|r| r.to_string())
            .unwrap_or(default_name.to_string())
    }

    fn new(schema: &Schema) -> Self {
        Self {
            query: Self::operation_name(OperationType::Query, "query", schema),
            mutation: Self::operation_name(OperationType::Mutation, "mutation", schema),
            subscription: Self::operation_name(OperationType::Subscription, "subscription", schema),
        }
    }

    fn name_for_operation_type(&self, operation_type: OperationType) -> &str {
        match operation_type {
            OperationType::Query => &self.query,
            OperationType::Mutation => &self.mutation,
            OperationType::Subscription => &self.subscription,
        }
    }
}

/// Limits the depth of the schema tree that is retained.
#[derive(Debug, Clone, Copy)]
pub enum DepthLimit {
    Unlimited,
    Limited(u32),
}

impl DepthLimit {
    /// Returns true if the depth limit has been reached.
    pub fn reached(&self) -> bool {
        match self {
            DepthLimit::Unlimited => false,
            DepthLimit::Limited(depth) => *depth == 0,
        }
    }

    /// Decrements the depth limit. This should be called when descending a level in the schema type tree.
    pub fn decrement(self) -> Self {
        match self {
            DepthLimit::Unlimited => self,
            DepthLimit::Limited(depth) => DepthLimit::Limited(depth - 1),
        }
    }
}

/// Tree shaker for GraphQL schemas
pub struct SchemaTreeShaker<'schema> {
    schema: &'schema Schema,
    named_type_nodes: HashMap<String, TreeTypeNode>,
    directive_nodes: HashMap<String, TreeDirectiveNode<'schema>>,
    operation_types: Vec<OperationType>,
    operation_type_names: RootOperationNames,
    named_fragments: HashMap<String, Node<FragmentDefinition>>,
}

struct TreeTypeNode {
    retain: bool,
    filtered_field: Option<Vec<String>>,
}

struct TreeDirectiveNode<'schema> {
    node: &'schema DirectiveDefinition,
    retain: bool,
}

impl<'schema> SchemaTreeShaker<'schema> {
    pub fn new(schema: &'schema Schema) -> Self {
        let mut named_type_nodes: HashMap<String, TreeTypeNode> = HashMap::default();
        let mut directive_nodes: HashMap<String, TreeDirectiveNode> = HashMap::default();

        schema.types.iter().for_each(|(_name, extended_type)| {
            let key = extended_type.name().as_str();
            if named_type_nodes.contains_key(key) {
                tracing::error!("type {} already exists", key);
            }
            named_type_nodes.insert(
                key.to_string(),
                TreeTypeNode {
                    filtered_field: None,
                    retain: false,
                },
            );
        });

        schema
            .directive_definitions
            .iter()
            .for_each(|(name, directive_def)| {
                let key = name.as_str();
                if directive_nodes.contains_key(key) {
                    tracing::error!("directive {} already exists", key);
                }
                directive_nodes.insert(
                    key.to_string(),
                    TreeDirectiveNode {
                        node: directive_def,
                        retain: false,
                    },
                );
            });

        Self {
            schema,
            named_type_nodes,
            directive_nodes,
            operation_types: Vec::default(),
            named_fragments: HashMap::default(),
            operation_type_names: RootOperationNames::new(schema),
        }
    }

    pub fn retain_operation_type(
        &mut self,
        operation_type: OperationType,
        selection_set: Option<&Vec<Selection>>,
        depth_limit: DepthLimit,
    ) {
        self.operation_types.push(operation_type);
        let operation_type_name = self
            .operation_type_names
            .name_for_operation_type(operation_type);

        if let Some(operation_type_extended_type) = self.schema.types.get(operation_type_name) {
            retain_type(
                self,
                operation_type_extended_type,
                selection_set,
                depth_limit,
            );
        } else {
            tracing::error!("root operation type {} not found in schema", operation_type);
        }
    }

    /// Retain a specific type, and recursively every type it references, up to a given depth.
    pub fn retain_type(&mut self, retain: &ExtendedType, depth_limit: DepthLimit) {
        retain_type(self, retain, None, depth_limit);
    }

    pub fn retain_operation(
        &mut self,
        operation: &OperationDefinition,
        document: &Document,
        depth_limit: DepthLimit,
    ) {
        self.named_fragments = document
            .definitions
            .iter()
            .filter_map(|def| match def {
                Definition::FragmentDefinition(fragment_def) => {
                    Some((fragment_def.name.to_string(), fragment_def.clone()))
                }
                _ => None,
            })
            .collect();
        self.retain_operation_type(
            operation.operation_type,
            Some(&operation.selection_set),
            depth_limit,
        )
    }

    /// Return the set of types retained after tree shaking.
    pub fn shaken(&mut self) -> Result<Schema, Box<WithErrors<Schema>>> {
        let root_operations = self
            .operation_types
            .iter()
            .filter_map(|operation_type| {
                self.schema
                    .root_operation(*operation_type)
                    .cloned()
                    .map(|operation_name| Node::new((*operation_type, operation_name)))
            })
            .collect();

        let schema_definition =
            Definition::SchemaDefinition(apollo_compiler::Node::new(SchemaDefinition {
                root_operations,
                description: self.schema.schema_definition.description.clone(),
                directives: DirectiveList(
                    self.schema
                        .schema_definition
                        .directives
                        .0
                        .iter()
                        .map(|directive| directive.node.clone())
                        .collect(),
                ),
            }));

        let directive_definitions = self
            .schema
            .directive_definitions
            .iter()
            .filter_map(|(directive_name, directive_def)| {
                self.directive_nodes
                    .get(directive_name.as_str())
                    .and_then(|n| {
                        (!directive_def.is_built_in() && n.retain)
                            .then_some(Definition::DirectiveDefinition(directive_def.clone()))
                    })
            })
            .collect();

        let type_definitions = self
            .schema
            .types
            .iter()
            .filter_map(|(_type_name, extended_type)| {
                if extended_type.is_built_in() {
                    None
                } else {
                    match extended_type {
                        ExtendedType::Object(object_def) => self
                            .named_type_nodes
                            .get(object_def.name.as_str())
                            .and_then(|tree_node| {
                                if tree_node.retain {
                                    Some(Definition::ObjectTypeDefinition(Node::new(
                                        ObjectTypeDefinition {
                                            description: object_def.description.clone(),
                                            directives: DirectiveList(
                                                object_def
                                                    .directives
                                                    .0
                                                    .iter()
                                                    .map(|directive| directive.node.clone())
                                                    .collect(),
                                            ),
                                            name: object_def.name.clone(),
                                            implements_interfaces: object_def
                                                .implements_interfaces
                                                .iter()
                                                .map(|implemented_interface| {
                                                    implemented_interface.name.clone()
                                                })
                                                .collect(),
                                            fields: object_def
                                                .fields
                                                .clone()
                                                .into_iter()
                                                .filter_map(|(field_name, field)| {
                                                    if let Some(filtered_fields) =
                                                        &tree_node.filtered_field
                                                    {
                                                        filtered_fields
                                                            .contains(&field_name.to_string())
                                                            .then_some(field.node)
                                                    } else {
                                                        Some(field.node)
                                                    }
                                                })
                                                .collect(),
                                        },
                                    )))
                                } else if self.schema.root_operation(OperationType::Query).is_some()
                                {
                                    None
                                } else {
                                    tracing::error!("object type {} not found", object_def.name);
                                    None
                                }
                            }),
                        ExtendedType::InputObject(input_def) => self
                            .named_type_nodes
                            .get(input_def.name.as_str())
                            .and_then(|tree_node| {
                                if tree_node.retain {
                                    Some(Definition::InputObjectTypeDefinition(Node::new(
                                        InputObjectTypeDefinition {
                                            description: input_def.description.clone(),
                                            directives: DirectiveList(
                                                input_def
                                                    .directives
                                                    .0
                                                    .iter()
                                                    .map(|directive| directive.node.clone())
                                                    .collect(),
                                            ),
                                            name: input_def.name.clone(),
                                            fields: input_def
                                                .fields
                                                .clone()
                                                .into_iter()
                                                .filter_map(|(field_name, field)| {
                                                    if let Some(filtered_fields) =
                                                        &tree_node.filtered_field
                                                    {
                                                        filtered_fields
                                                            .contains(&field_name.to_string())
                                                            .then_some(field.node)
                                                    } else {
                                                        Some(field.node)
                                                    }
                                                })
                                                .collect(),
                                        },
                                    )))
                                } else {
                                    None
                                }
                            }),
                        ExtendedType::Interface(interface_def) => self
                            .named_type_nodes
                            .get(interface_def.name.as_str())
                            .and_then(|tree_node| {
                                if tree_node.retain {
                                    Some(Definition::InterfaceTypeDefinition(Node::new(
                                        InterfaceTypeDefinition {
                                            description: interface_def.description.clone(),
                                            directives: DirectiveList(
                                                interface_def
                                                    .directives
                                                    .0
                                                    .iter()
                                                    .map(|directive| directive.node.clone())
                                                    .collect(),
                                            ),
                                            name: interface_def.name.clone(),
                                            implements_interfaces: interface_def
                                                .implements_interfaces
                                                .iter()
                                                .map(|implemented_interface| {
                                                    implemented_interface.name.clone()
                                                })
                                                .collect(),
                                            fields: interface_def
                                                .fields
                                                .clone()
                                                .into_iter()
                                                .filter_map(|(field_name, field)| {
                                                    if let Some(filtered_fields) =
                                                        &tree_node.filtered_field
                                                    {
                                                        filtered_fields
                                                            .contains(&field_name.to_string())
                                                            .then_some(field.node)
                                                    } else {
                                                        Some(field.node)
                                                    }
                                                })
                                                .collect(),
                                        },
                                    )))
                                } else {
                                    None
                                }
                            }),
                        ExtendedType::Union(union_def) => self
                            .named_type_nodes
                            .get(union_def.name.as_str())
                            .is_some_and(|n| n.retain)
                            .then(|| {
                                Definition::UnionTypeDefinition(Node::new(UnionTypeDefinition {
                                    description: union_def.description.clone(),
                                    directives: DirectiveList(
                                        union_def
                                            .directives
                                            .0
                                            .iter()
                                            .map(|directive| directive.node.clone())
                                            .collect(),
                                    ),
                                    name: union_def.name.clone(),
                                    members: union_def
                                        .members
                                        .clone()
                                        .into_iter()
                                        .filter_map(|member| {
                                            if let Some(member_tree_node) =
                                                self.named_type_nodes.get(member.as_str())
                                            {
                                                member_tree_node.retain.then_some(member.name)
                                            } else {
                                                tracing::error!(
                                                    "union member {} not found",
                                                    member
                                                );
                                                None
                                            }
                                        })
                                        .collect(),
                                }))
                            }),
                        ExtendedType::Enum(enum_def) => self
                            .named_type_nodes
                            .get(enum_def.name.as_str())
                            .and_then(|tree_node| {
                                if tree_node.retain {
                                    Some(Definition::EnumTypeDefinition(Node::new(
                                        EnumTypeDefinition {
                                            description: enum_def.description.clone(),
                                            directives: DirectiveList(
                                                enum_def
                                                    .directives
                                                    .0
                                                    .iter()
                                                    .map(|directive| directive.node.clone())
                                                    .collect(),
                                            ),
                                            name: enum_def.name.clone(),
                                            values: enum_def
                                                .values
                                                .iter()
                                                .map(|(_enum_value_name, enum_value)| {
                                                    enum_value.node.clone()
                                                })
                                                .collect(),
                                        },
                                    )))
                                } else {
                                    None
                                }
                            }),
                        ExtendedType::Scalar(scalar_def) => self
                            .named_type_nodes
                            .get(scalar_def.name.as_str())
                            .and_then(|tree_node| {
                                if tree_node.retain {
                                    Some(Definition::ScalarTypeDefinition(Node::new(
                                        ScalarTypeDefinition {
                                            description: scalar_def.description.clone(),
                                            directives: DirectiveList(
                                                scalar_def
                                                    .directives
                                                    .0
                                                    .iter()
                                                    .map(|directive| directive.node.clone())
                                                    .collect(),
                                            ),
                                            name: scalar_def.name.clone(),
                                        },
                                    )))
                                } else {
                                    None
                                }
                            }),
                    }
                }
            })
            .collect();

        let mut document = Document::new();
        document.definitions = [
            // // TODO: don't push if theres no data
            vec![schema_definition],
            directive_definitions,
            type_definitions,
        ]
        .concat();

        document.to_schema().map_err(Box::new)
    }
}

fn selection_set_to_fields(
    selection_set: &Selection,
    named_fragments: &HashMap<String, Node<FragmentDefinition>>,
) -> Vec<Node<Field>> {
    match selection_set {
        Selection::Field(field) => {
            if field.name == "__typename" {
                vec![]
            } else {
                vec![field.clone()]
            }
        }
        Selection::FragmentSpread(fragment) => named_fragments
            .get(fragment.fragment_name.as_str())
            .map(|f| {
                f.selection_set
                    .iter()
                    .flat_map(|s| selection_set_to_fields(s, named_fragments))
                    .collect()
            })
            .unwrap_or_default(),
        Selection::InlineFragment(fragment) => fragment
            .selection_set
            .iter()
            .flat_map(|s| selection_set_to_fields(s, named_fragments))
            .collect(),
    }
}

fn retain_type(
    tree_shaker: &mut SchemaTreeShaker,
    extended_type: &ExtendedType,
    selection_set: Option<&Vec<Selection>>,
    depth_limit: DepthLimit,
) {
    // Check if we've exceeded the depth limit
    if depth_limit.reached() {
        return;
    }

    let type_name = extended_type.name().as_str();
    let selected_fields = if let Some(selection_set) = selection_set {
        let selected_fields = selection_set
            .iter()
            .flat_map(|s| selection_set_to_fields(s, &tree_shaker.named_fragments))
            .collect::<Vec<_>>();

        Some(selected_fields)
    } else {
        None
    };

    if let Some(tree_node) = tree_shaker.named_type_nodes.get_mut(type_name) {
        // If we have already visited this node, early return to avoid infinite recursion.
        // depth_limit and selection_set both have inherent exit cases and may add more types with multiple passes, so never early return for them.
        if tree_node.retain
            && selection_set.is_none()
            && matches!(depth_limit, DepthLimit::Unlimited)
        {
            return;
        }

        tree_node.retain = true;
        if let Some(selected_fields) = selected_fields.as_ref() {
            let additional_fields = selected_fields
                .iter()
                .map(|f| f.name.to_string())
                .collect::<Vec<_>>();

            tree_node.filtered_field = Some(
                [
                    tree_node.filtered_field.clone().unwrap_or_default(),
                    additional_fields,
                ]
                .concat(),
            );
        }
    }

    extended_type
        .directives()
        .iter()
        .for_each(|t| retain_directive(tree_shaker, t.name.as_str(), depth_limit));

    match extended_type {
        ExtendedType::Object(def) => {
            selected_fields
                .as_ref()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.as_str(),
                                def.fields.get(field.name.as_str()),
                                Some(&field.directives),
                                Some(&field.selection_set),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or(
                    def.fields
                        .iter()
                        .map(|(name, field_definition)| {
                            (name.as_str(), Some(field_definition), None, None)
                        })
                        .collect::<Vec<_>>(),
                )
                .into_iter()
                .for_each(
                    |(
                        field_name,
                        field_definition,
                        field_selection_directives,
                        field_selection_set,
                    )| {
                        if let Some(field_type) = field_definition {
                            let field_type_name = field_type.ty.inner_named_type();
                            if let Some(field_type_def) =
                                tree_shaker.schema.types.get(field_type_name)
                            {
                                retain_type(
                                    tree_shaker,
                                    field_type_def,
                                    field_selection_set,
                                    depth_limit.decrement(),
                                );
                            } else {
                                tracing::error!("field type {} not found", field_type_name);
                            }

                            field_type.arguments.iter().for_each(|arg| {
                                let arg_type_name = arg.ty.inner_named_type();
                                if let Some(arg_type) = tree_shaker.schema.types.get(arg_type_name)
                                {
                                    retain_type(
                                        tree_shaker,
                                        arg_type,
                                        None,
                                        depth_limit.decrement(),
                                    );
                                } else {
                                    tracing::error!(
                                        "field argument type {} not found",
                                        arg_type_name
                                    );
                                }
                            });
                        } else {
                            tracing::error!("field {} not found", field_name);
                        }

                        if let Some(field_definition_directives) =
                            field_definition.map(|f| f.directives.clone())
                        {
                            field_definition_directives.iter().for_each(|directive| {
                                retain_directive(tree_shaker, directive.name.as_str(), depth_limit);
                            })
                        }
                        if let Some(field_selection_directives) = field_selection_directives {
                            field_selection_directives.iter().for_each(|directive| {
                                retain_directive(tree_shaker, directive.name.as_str(), depth_limit);
                            })
                        }
                    },
                );
        }
        ExtendedType::Interface(def) => {
            selected_fields
                .as_ref()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.as_str(),
                                def.fields.get(field.name.as_str()),
                                Some(&field.directives),
                                Some(&field.selection_set),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or(
                    def.fields
                        .iter()
                        .map(|(name, field_definition)| {
                            (name.as_str(), Some(field_definition), None, None)
                        })
                        .collect::<Vec<_>>(),
                )
                .into_iter()
                .for_each(
                    |(
                        field_name,
                        field_definition,
                        field_selection_directives,
                        field_selection_set,
                    )| {
                        if let Some(field_type) = field_definition {
                            let field_type_name = field_type.ty.inner_named_type();
                            if let Some(field_type_def) =
                                tree_shaker.schema.types.get(field_type_name)
                            {
                                retain_type(
                                    tree_shaker,
                                    field_type_def,
                                    field_selection_set,
                                    depth_limit.decrement(),
                                );
                            } else {
                                tracing::error!("field type {} not found", field_type_name);
                            }

                            field_type.arguments.iter().for_each(|arg| {
                                let arg_type_name = arg.ty.inner_named_type();
                                if let Some(arg_type) = tree_shaker.schema.types.get(arg_type_name)
                                {
                                    retain_type(
                                        tree_shaker,
                                        arg_type,
                                        None,
                                        depth_limit.decrement(),
                                    );
                                } else {
                                    tracing::error!(
                                        "field argument type {} not found",
                                        arg_type_name
                                    );
                                }
                            });
                        } else {
                            tracing::error!("field {} not found", field_name);
                        }

                        if let Some(field_definition_directives) =
                            field_definition.map(|f| f.directives.clone())
                        {
                            field_definition_directives.iter().for_each(|directive| {
                                retain_directive(tree_shaker, directive.name.as_str(), depth_limit);
                            })
                        }
                        if let Some(field_selection_directives) = field_selection_directives {
                            field_selection_directives.iter().for_each(|directive| {
                                retain_directive(tree_shaker, directive.name.as_str(), depth_limit);
                            })
                        }
                    },
                );
        }
        ExtendedType::Union(union_def) => union_def.members.iter().for_each(|member| {
            if let Some(member_type) = tree_shaker.schema.types.get(member.as_str()) {
                let member_selection_set = selection_set
                    .map(|selection_set| {
                        selection_set
                            .clone()
                            .into_iter()
                            .filter(|selection| match selection {
                                Selection::Field(_) => true,
                                Selection::FragmentSpread(fragment) => {
                                    if let Some(fragment_def) = &tree_shaker
                                        .named_fragments
                                        .get(fragment.fragment_name.as_str())
                                    {
                                        fragment_def.type_condition == member.as_str()
                                    } else {
                                        tracing::error!(
                                            "fragment {} not found",
                                            fragment.fragment_name
                                        );
                                        false
                                    }
                                }
                                Selection::InlineFragment(fragment) => fragment
                                    .type_condition
                                    .clone()
                                    .is_none_or(|type_condition| {
                                        type_condition.as_str() == member.as_str()
                                    }),
                            })
                            .collect::<Vec<Selection>>()
                    })
                    .and_then(|s| if s.is_empty() { None } else { Some(s) });

                if selection_set.is_none() || member_selection_set.is_some() {
                    retain_type(
                        tree_shaker,
                        member_type,
                        member_selection_set.as_ref(),
                        depth_limit.decrement(),
                    );
                }
            } else {
                tracing::error!("union member {} not found", member);
            }
        }),
        ExtendedType::Enum(def) => def.values.iter().for_each(|(_name, value)| {
            value.directives.iter().for_each(|directive| {
                retain_directive(tree_shaker, directive.name.as_str(), depth_limit);
            })
        }),
        ExtendedType::Scalar(_) => {}
        ExtendedType::InputObject(input_def) => {
            input_def
                .fields
                .iter()
                .for_each(|(_name, field_definition)| {
                    let field_type_name = field_definition.ty.inner_named_type();
                    if let Some(field_type_def) = tree_shaker.schema.types.get(field_type_name) {
                        retain_type(tree_shaker, field_type_def, None, depth_limit.decrement());
                    } else {
                        tracing::error!("field type {} not found", field_type_name);
                    }
                    field_definition.directives.iter().for_each(|directive| {
                        retain_directive(tree_shaker, directive.name.as_str(), depth_limit)
                    });
                });
        }
    }
}

fn retain_directive(
    tree_shaker: &mut SchemaTreeShaker,
    directive_name: &str,
    depth_limit: DepthLimit,
) {
    if let Some(tree_directive_node) = tree_shaker.directive_nodes.get_mut(directive_name) {
        tree_directive_node.retain = true;
        tree_directive_node.node.arguments.iter().for_each(|arg| {
            let arg_type_name = arg.ty.inner_named_type();
            if let Some(arg_type) = tree_shaker.schema.types.get(arg_type_name) {
                retain_type(tree_shaker, arg_type, None, depth_limit.decrement())
            } else {
                tracing::error!("argument type {} not found", arg_type_name);
            }
        });
    }
}

#[cfg(test)]
mod test {
    use apollo_compiler::{ast::OperationType, parser::Parser};
    use rstest::{fixture, rstest};

    use crate::{
        operations::operation_defs,
        schema_tree_shake::{DepthLimit, SchemaTreeShaker},
    };

    #[test]
    fn should_remove_type_mutation_mode_none() {
        let source_text = r#"
            type Query { id: String }
            type Mutation { id: String }
            type Subscription { id: String }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "type Query {\n  id: String\n}\n"
        );
    }

    #[test]
    fn should_remove_type_mutation_mode_all() {
        let source_text = r#"
            type Query { id: String }
            type Mutation { id: String }
            type Subscription { id: String }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Unlimited);
        shaker.retain_operation_type(OperationType::Mutation, None, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "type Query {\n  id: String\n}\n\ntype Mutation {\n  id: String\n}\n"
        );
    }

    #[test]
    fn should_remove_custom_names_mutation_mode_none() {
        let source_text = r#"
            schema {
              query: CustomQuery,
              mutation: CustomMutation,
              subscription: CustomSubscription
            }
            type CustomQuery { id: String }
            type CustomMutation { id: String }
            type CustomSubscription { id: String }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "schema {\n  query: CustomQuery\n}\n\ntype CustomQuery {\n  id: String\n}\n"
        );
    }

    #[test]
    fn should_remove_custom_names_mutation_mode_all() {
        let source_text = r#"
            schema {
              query: CustomQuery,
              mutation: CustomMutation,
              subscription: CustomSubscription
            }
            type CustomQuery { id: String }
            type CustomMutation { id: String }
            type CustomSubscription { id: String }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Unlimited);
        shaker.retain_operation_type(OperationType::Mutation, None, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "schema {\n  query: CustomQuery\n  mutation: CustomMutation\n}\n\ntype CustomQuery {\n  id: String\n}\n\ntype CustomMutation {\n  id: String\n}\n"
        );
    }

    #[test]
    fn should_remove_orphan_types() {
        let source_text = r#"
            type Query { id: UsedInQuery }
            type Mutation { id: UsedInMutation }
            type Subscription { id: UsedInSubscription }
            scalar UsedInQuery
            type UsedInMutation { id: String }
            enum UsedInSubscription { VALUE }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Unlimited);
        shaker.retain_operation_type(OperationType::Mutation, None, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "type Query {\n  id: UsedInQuery\n}\n\ntype Mutation {\n  id: UsedInMutation\n}\n\nscalar UsedInQuery\n\ntype UsedInMutation {\n  id: String\n}\n"
        );
    }

    #[test]
    fn should_work_with_selection_set() {
        let source_text = r#"
            type Query { id: UsedInQuery unused: UsedInQueryButUnusedField }
            type Mutation { id: UsedInMutation }
            type Subscription { id: UsedInSubscription }
            scalar UsedInQuery
            type UsedInQueryButUnusedField { id: String, unused: String }
            type UsedInMutation { id: String }
            enum UsedInSubscription { VALUE }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        let (operation_document, operation_def, _comments) = operation_defs(
            "query TestQuery { id }",
            false,
            Some("operation.graphql".to_string()),
        )
        .unwrap()
        .unwrap();
        shaker.retain_operation(&operation_def, &operation_document, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "type Query {\n  id: UsedInQuery\n}\n\nscalar UsedInQuery\n"
        );
    }

    #[fixture]
    fn nested_schema() -> apollo_compiler::Schema {
        Parser::new()
            .parse_ast(
                r#"
                    type Query  { level1: Level1 }
                    type Level1 { level2: Level2 }
                    type Level2 { level3: Level3 }
                    type Level3 { level4: Level4 }
                    type Level4 { id: String }
                "#,
                "schema.graphql",
            )
            .unwrap()
            .to_schema_validate()
            .unwrap()
            .into_inner()
    }

    #[rstest]
    fn should_respect_depth_limit(nested_schema: apollo_compiler::Schema) {
        let mut shaker = SchemaTreeShaker::new(&nested_schema);

        // Get the Query type to start from
        let query_type = nested_schema.types.get("Query").unwrap();

        // Test with depth limit of 1
        shaker.retain_type(query_type, DepthLimit::Limited(1));
        let shaken_schema = shaker.shaken().unwrap();

        // Should retain only Query, not Level1, Level2, Level3, or Level4
        assert!(shaken_schema.types.contains_key("Query"));
        assert!(!shaken_schema.types.contains_key("Level1"));
        assert!(!shaken_schema.types.contains_key("Level2"));
        assert!(!shaken_schema.types.contains_key("Level3"));
        assert!(!shaken_schema.types.contains_key("Level4"));

        // Test with depth limit of 2
        let mut shaker = SchemaTreeShaker::new(&nested_schema);
        shaker.retain_type(query_type, DepthLimit::Limited(2));
        let shaken_schema = shaker.shaken().unwrap();

        // Should retain Query and Level1, but not deeper levels
        assert!(shaken_schema.types.contains_key("Query"));
        assert!(shaken_schema.types.contains_key("Level1"));
        assert!(!shaken_schema.types.contains_key("Level2"));
        assert!(!shaken_schema.types.contains_key("Level3"));
        assert!(!shaken_schema.types.contains_key("Level4"));

        // Test with depth limit of 1 starting from Level2
        let mut shaker = SchemaTreeShaker::new(&nested_schema);
        let level2_type = nested_schema.types.get("Level2").unwrap();
        shaker.retain_type(level2_type, DepthLimit::Limited(1));
        let shaken_schema = shaker.shaken().unwrap();

        // Should retain only Level2
        assert!(!shaken_schema.types.contains_key("Level1"));
        assert!(shaken_schema.types.contains_key("Level2"));
        assert!(!shaken_schema.types.contains_key("Level3"));
        assert!(!shaken_schema.types.contains_key("Level4"));

        // Test with depth limit of 2 starting from Level2
        let mut shaker = SchemaTreeShaker::new(&nested_schema);
        shaker.retain_type(level2_type, DepthLimit::Limited(2));
        let shaken_schema = shaker.shaken().unwrap();

        // Should retain Level2 and Level3
        assert!(!shaken_schema.types.contains_key("Level1"));
        assert!(shaken_schema.types.contains_key("Level2"));
        assert!(shaken_schema.types.contains_key("Level3"));
        assert!(!shaken_schema.types.contains_key("Level4"));

        // Test with depth limit of 5 starting from Level2
        let mut shaker = SchemaTreeShaker::new(&nested_schema);
        shaker.retain_type(level2_type, DepthLimit::Limited(5));
        let shaken_schema = shaker.shaken().unwrap();

        // Should retain Level2 and deeper types
        assert!(!shaken_schema.types.contains_key("Level1"));
        assert!(shaken_schema.types.contains_key("Level2"));
        assert!(shaken_schema.types.contains_key("Level3"));
        assert!(shaken_schema.types.contains_key("Level4"));
    }

    #[rstest]
    fn should_retain_all_types_with_unlimited_depth(nested_schema: apollo_compiler::Schema) {
        let mut shaker = SchemaTreeShaker::new(&nested_schema);

        // Get the Query type to start from
        let query_type = nested_schema.types.get("Query").unwrap();

        // Test with unlimited depth
        shaker.retain_type(query_type, DepthLimit::Unlimited);
        let shaken_schema = shaker.shaken().unwrap();

        // Should retain all types
        assert!(shaken_schema.types.contains_key("Query"));
        assert!(shaken_schema.types.contains_key("Level1"));
        assert!(shaken_schema.types.contains_key("Level2"));
        assert!(shaken_schema.types.contains_key("Level3"));
        assert!(shaken_schema.types.contains_key("Level4"));
    }

    #[test]
    fn should_work_with_recursive_schemas() {
        let source_text = r#"
            type Query { id: TypeA }
            type TypeA { id: TypeB }
            type TypeB { id: TypeA }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "type Query {\n  id: TypeA\n}\n\ntype TypeA {\n  id: TypeB\n}\n\ntype TypeB {\n  id: TypeA\n}\n"
        );
    }

    #[test]
    fn should_work_with_recursive_and_depth() {
        let source_text = r#"
            type Query { field1: TypeA, field2: TypeB }
            type TypeA { id: TypeB }
            type TypeB { id: TypeC }
            type TypeC { id: TypeA }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Limited(3));
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "type Query {\n  field1: TypeA\n  field2: TypeB\n}\n\ntype TypeA {\n  id: TypeB\n}\n\ntype TypeB {\n  id: TypeC\n}\n\ntype TypeC {\n  id: TypeA\n}\n"
        );
    }

    #[test]
    fn should_retain_builtin_directives() {
        let source_text = r#"
            type Query {
                field1: String @deprecated(reason: "Use 'field2' instead")
                field2: String
            }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Limited(3));
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "type Query {\n  field1: String @deprecated(reason: \"Use 'field2' instead\")\n  field2: String\n}\n"
        );
    }

    #[test]
    fn should_retain_custom_directives() {
        let source_text = r#"
            type Query {
                field1: String @CustomDirective(arg: "Use 'field2' instead")
                field2: String
            }
            directive @CustomDirective(arg: CustomScalar) on FIELD_DEFINITION
            scalar CustomScalar
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Limited(3));
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "directive @CustomDirective(arg: CustomScalar) on FIELD_DEFINITION\n\ntype Query {\n  field1: String @CustomDirective(arg: \"Use 'field2' instead\")\n  field2: String\n}\n\nscalar CustomScalar\n"
        );
    }

    #[test]
    fn recursive_input() {
        let source_text = r#"
            input Filter {
                field: String
                filter: Filter
            }
            type Query {
                field(filter: Filter): String
            }
        "#;
        let document = Parser::new()
            .parse_ast(source_text, "schema.graphql")
            .unwrap();
        let schema = document.to_schema_validate().unwrap();
        let mut shaker = SchemaTreeShaker::new(&schema);
        shaker.retain_operation_type(OperationType::Query, None, DepthLimit::Unlimited);
        assert_eq!(
            shaker.shaken().unwrap().to_string(),
            "input Filter {\n  field: String\n  filter: Filter\n}\n\ntype Query {\n  field(filter: Filter): String\n}\n"
        );
    }
}
