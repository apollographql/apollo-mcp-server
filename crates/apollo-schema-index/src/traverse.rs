//! Helpers for extracting searchable text from a GraphQL schema's types.

use apollo_compiler::Schema;
use apollo_compiler::ast::NamedType;
use apollo_compiler::schema::ExtendedType;
use std::collections::HashSet;

/// Collect field-name + description text for `return_type`, walked `depth` levels deep.
/// Cycle-guarded; scalars/enums/unions and depth 0 yield "".
pub(crate) fn flatten_return_type(schema: &Schema, return_type: &str, depth: usize) -> String {
    let mut out = String::new();
    let mut visited: HashSet<String> = HashSet::new();
    collect(schema, return_type, depth, &mut visited, &mut out);
    out.trim().to_string()
}

fn collect(
    schema: &Schema,
    type_name: &str,
    depth: usize,
    visited: &mut HashSet<String>,
    out: &mut String,
) {
    if depth == 0 || !visited.insert(type_name.to_string()) {
        return;
    }
    let named = NamedType::new_unchecked(type_name);
    let Some(ExtendedType::Object(obj)) = schema.types.get(&named) else {
        return; // only object types contribute nested field text
    };
    for (name, field) in obj.fields.iter() {
        out.push(' ');
        out.push_str(name.as_str());
        if let Some(desc) = field.description.as_ref() {
            out.push(' ');
            out.push_str(desc.as_str());
        }
        let inner = field.ty.inner_named_type();
        collect(schema, inner.as_str(), depth - 1, visited, out);
    }
}

#[cfg(test)]
mod flatten_tests {
    use super::*;
    use apollo_compiler::Schema;

    const S: &str = r#"
        type Query { a: Foo }
        type Foo { bar: String "documented" baz: Bar }
        type Bar { deep: String }
    "#;

    #[test]
    fn flatten_depth_1_includes_direct_fields_only() {
        let schema = Schema::parse(S, "s.graphql").unwrap().validate().unwrap();
        let text = flatten_return_type(&schema, "Foo", 1);
        assert!(text.contains("bar"));
        assert!(text.contains("baz"));
        assert!(!text.contains("deep")); // depth 1 does not descend into Bar
    }

    #[test]
    fn flatten_depth_0_is_empty() {
        let schema = Schema::parse(S, "s.graphql").unwrap().validate().unwrap();
        assert_eq!(flatten_return_type(&schema, "Foo", 0), "");
    }

    #[test]
    fn flatten_handles_cycles() {
        let cyclic = r#"type Query { a: Node } type Node { next: Node name: String }"#;
        let schema = Schema::parse(cyclic, "c.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let text = flatten_return_type(&schema, "Node", 5);
        assert!(text.contains("next"));
        assert!(text.contains("name"));
    }
}
