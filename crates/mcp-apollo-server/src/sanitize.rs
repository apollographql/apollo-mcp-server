//! Provide sanitized type definitions suitable for an AI model.
//! For example, remove directives from GraphQL schema types.
use apollo_compiler::schema::{EnumType, FieldDefinition, ObjectType, ScalarType, UnionType};
use apollo_compiler::{ast, schema};

pub trait Sanitize<T> {
    fn sanitize(self) -> T;
}

// Implementation for all schema directive types
macro_rules! impl_sanitize {
    ($type:ty, $directive_list_type:path) => {
        impl Sanitize<$type> for $type {
            fn sanitize(self) -> Self {
                Self {
                    directives: $directive_list_type(vec![]),
                    ..self
                }
            }
        }
    };
}

impl_sanitize!(EnumType, schema::DirectiveList);
impl_sanitize!(FieldDefinition, ast::DirectiveList);
impl_sanitize!(ObjectType, schema::DirectiveList);
impl_sanitize!(UnionType, schema::DirectiveList);
impl_sanitize!(ScalarType, schema::DirectiveList);
