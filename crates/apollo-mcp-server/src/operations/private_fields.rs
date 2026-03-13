use std::collections::HashMap;

use apollo_compiler::{
    Node,
    ast::{Definition, Document, FragmentDefinition, Selection},
    parser::Parser,
};
use serde_json::Value;

const PRIVATE_DIRECTIVE_NAME: &str = "private";

/// A tree of field paths marked as `@private`.
///
/// If `is_private` is true, this node and all its children are private.
/// The tree mirrors the response JSON structure using response keys (alias or field name).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PrivateFieldTree {
    pub(crate) is_private: bool,
    pub(crate) children: HashMap<String, PrivateFieldTree>,
}

impl PrivateFieldTree {
    pub(crate) fn has_private_fields(&self) -> bool {
        self.is_private || !self.children.is_empty()
    }
}

/// Walk the selection set and build a tree of field paths marked `@private`.
pub(crate) fn collect_private_fields(
    selection_set: &[Selection],
    named_fragments: &HashMap<String, Node<FragmentDefinition>>,
) -> PrivateFieldTree {
    let mut tree = PrivateFieldTree::default();
    collect_from_selections(selection_set, named_fragments, &mut tree);
    tree
}

fn collect_from_selections(
    selection_set: &[Selection],
    named_fragments: &HashMap<String, Node<FragmentDefinition>>,
    tree: &mut PrivateFieldTree,
) {
    for selection in selection_set {
        match selection {
            Selection::Field(field) => {
                let response_key = field
                    .alias
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| field.name.to_string());

                if field
                    .directives
                    .0
                    .iter()
                    .any(|d| d.name == PRIVATE_DIRECTIVE_NAME)
                {
                    tree.children.entry(response_key).or_default().is_private = true;
                } else if !field.selection_set.is_empty() {
                    let mut child = PrivateFieldTree::default();
                    collect_from_selections(&field.selection_set, named_fragments, &mut child);
                    if child.has_private_fields() {
                        tree.children.insert(response_key, child);
                    }
                }
            }
            Selection::FragmentSpread(spread) => {
                if let Some(fragment) = named_fragments.get(spread.fragment_name.as_str()) {
                    collect_from_selections(&fragment.selection_set, named_fragments, tree);
                }
            }
            Selection::InlineFragment(inline) => {
                collect_from_selections(&inline.selection_set, named_fragments, tree);
            }
        }
    }
}

/// Clone the document and remove all `@private` directives from field selections.
pub(crate) fn strip_private_directives(document: &Document) -> Document {
    let mut doc = document.clone();
    for def in &mut doc.definitions {
        match def {
            Definition::OperationDefinition(op) => {
                let op = Node::make_mut(op);
                strip_from_selection_set(&mut op.selection_set);
            }
            Definition::FragmentDefinition(frag) => {
                let frag = Node::make_mut(frag);
                strip_from_selection_set(&mut frag.selection_set);
            }
            _ => {}
        }
    }
    doc
}

fn strip_from_selection_set(selections: &mut [Selection]) {
    for selection in selections.iter_mut() {
        match selection {
            Selection::Field(field) => {
                let field = Node::make_mut(field);
                field
                    .directives
                    .0
                    .retain(|d| d.name != PRIVATE_DIRECTIVE_NAME);
                strip_from_selection_set(&mut field.selection_set);
            }
            Selection::InlineFragment(inline) => {
                let inline = Node::make_mut(inline);
                strip_from_selection_set(&mut inline.selection_set);
            }
            Selection::FragmentSpread(_) => {}
        }
    }
}

/// Parse a raw query string for `@private` directives.
///
/// Returns `Some((stripped_query, tree))` if private fields are found,
/// where `stripped_query` has `@private` directives removed.
/// Returns `None` if parsing fails or there are no `@private` fields.
pub(crate) fn process_private_directives(query: &str) -> Option<(String, PrivateFieldTree)> {
    // Parse failure returns None (no filtering). The invalid query will still be
    // forwarded and will fail at the GraphQL endpoint with a proper error.
    let document = Parser::new().parse_ast(query, "query.graphql").ok()?;

    let operation = document.definitions.iter().find_map(|def| match def {
        Definition::OperationDefinition(op) => Some(op),
        _ => None,
    })?;

    let named_fragments = collect_named_fragments(&document);
    let tree = collect_private_fields(&operation.selection_set, &named_fragments);

    if !tree.has_private_fields() {
        return None;
    }

    let stripped_doc = strip_private_directives(&document);
    let stripped_query = stripped_doc.serialize().no_indent().to_string();

    Some((stripped_query, tree))
}

/// Collect named fragments from a document into a lookup map.
pub(crate) fn collect_named_fragments(
    document: &Document,
) -> HashMap<String, Node<FragmentDefinition>> {
    document
        .definitions
        .iter()
        .filter_map(|def| match def {
            Definition::FragmentDefinition(frag) => Some((frag.name.to_string(), frag.clone())),
            _ => None,
        })
        .collect()
}

/// Filter private fields from a GraphQL JSON response.
///
/// Only the `data` key is filtered; `errors`, `extensions`, etc. are passed through.
pub(crate) fn filter_private_fields(response: &Value, tree: &PrivateFieldTree) -> Value {
    match response {
        Value::Object(map) => {
            let mut filtered = serde_json::Map::new();
            for (key, value) in map {
                if key == "data" {
                    filtered.insert(key.clone(), filter_object(value, tree));
                } else {
                    filtered.insert(key.clone(), value.clone());
                }
            }
            Value::Object(filtered)
        }
        other => other.clone(),
    }
}

fn filter_object(value: &Value, tree: &PrivateFieldTree) -> Value {
    match value {
        Value::Object(map) => {
            let mut filtered = serde_json::Map::new();
            for (key, val) in map {
                if let Some(child) = tree.children.get(key) {
                    if child.is_private {
                        continue;
                    }
                    filtered.insert(key.clone(), filter_object(val, child));
                } else {
                    filtered.insert(key.clone(), val.clone());
                }
            }
            Value::Object(filtered)
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|item| filter_object(item, tree)).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::parser::Parser;

    use super::*;

    fn parse_and_collect(query: &str) -> PrivateFieldTree {
        let doc = Parser::new().parse_ast(query, "test.graphql").unwrap();
        let (operation, named_fragments) = extract_operation_and_fragments(&doc);
        collect_private_fields(&operation.selection_set, &named_fragments)
    }

    fn extract_operation_and_fragments(
        doc: &Document,
    ) -> (
        &Node<apollo_compiler::ast::OperationDefinition>,
        HashMap<String, Node<FragmentDefinition>>,
    ) {
        let operation = doc
            .definitions
            .iter()
            .find_map(|def| match def {
                Definition::OperationDefinition(op) => Some(op),
                _ => None,
            })
            .unwrap();

        let fragments = doc
            .definitions
            .iter()
            .filter_map(|def| match def {
                Definition::FragmentDefinition(frag) => Some((frag.name.to_string(), frag.clone())),
                _ => None,
            })
            .collect();

        (operation, fragments)
    }

    #[test]
    fn no_private_fields() {
        let tree = parse_and_collect("query Q { fieldA fieldB fieldC }");
        assert!(!tree.has_private_fields());
    }

    #[test]
    fn flat_private_field() {
        let tree = parse_and_collect("query Q { fieldA fieldB @private fieldC }");
        assert!(tree.has_private_fields());
        assert!(tree.children.get("fieldB").unwrap().is_private);
        assert!(!tree.children.contains_key("fieldA"));
        assert!(!tree.children.contains_key("fieldC"));
    }

    #[test]
    fn nested_private_field() {
        let tree = parse_and_collect("query Q { user { name email @private } }");
        assert!(tree.has_private_fields());
        let user = tree.children.get("user").unwrap();
        assert!(!user.is_private);
        assert!(user.children.get("email").unwrap().is_private);
    }

    #[test]
    fn parent_marked_private_makes_subtree_private() {
        let tree = parse_and_collect("query Q { user @private { name email } }");
        assert!(tree.has_private_fields());
        assert!(tree.children.get("user").unwrap().is_private);
    }

    #[test]
    fn aliased_field_uses_alias_as_key() {
        let tree = parse_and_collect("query Q { myField: fieldB @private }");
        assert!(tree.has_private_fields());
        assert!(tree.children.get("myField").unwrap().is_private);
        assert!(!tree.children.contains_key("fieldB"));
    }

    #[test]
    fn fragment_spread_private_field() {
        let tree = parse_and_collect(
            "query Q { ...UserFields } fragment UserFields on Query { name email @private }",
        );
        assert!(tree.has_private_fields());
        assert!(tree.children.get("email").unwrap().is_private);
    }

    #[test]
    fn inline_fragment_private_field() {
        let tree = parse_and_collect("query Q { ... on Query { name email @private } }");
        assert!(tree.has_private_fields());
        assert!(tree.children.get("email").unwrap().is_private);
    }

    #[test]
    fn strip_removes_private_directive() {
        let doc = Parser::new()
            .parse_ast("query Q { fieldA fieldB @private fieldC }", "test.graphql")
            .unwrap();
        let stripped = strip_private_directives(&doc);
        let serialized = stripped.serialize().no_indent().to_string();
        assert!(!serialized.contains("@private"));
        assert!(serialized.contains("fieldA"));
        assert!(serialized.contains("fieldB"));
        assert!(serialized.contains("fieldC"));
    }

    #[test]
    fn strip_preserves_other_directives() {
        let doc = Parser::new()
            .parse_ast(
                "query Q($flag: Boolean) { fieldA @skip(if: $flag) fieldB @private }",
                "test.graphql",
            )
            .unwrap();
        let stripped = strip_private_directives(&doc);
        let serialized = stripped.serialize().no_indent().to_string();
        assert!(serialized.contains("@skip"));
        assert!(!serialized.contains("@private"));
    }

    #[test]
    fn strip_nested_private_directive() {
        let doc = Parser::new()
            .parse_ast("query Q { user { name email @private } }", "test.graphql")
            .unwrap();
        let stripped = strip_private_directives(&doc);
        let serialized = stripped.serialize().no_indent().to_string();
        assert!(!serialized.contains("@private"));
        assert!(serialized.contains("email"));
    }

    #[test]
    fn filter_flat_response() {
        let tree = parse_and_collect("query Q { fieldA fieldB @private fieldC }");
        let response = serde_json::json!({
            "data": { "fieldA": "a", "fieldB": "b", "fieldC": "c" }
        });
        let filtered = filter_private_fields(&response, &tree);
        let data = filtered.get("data").unwrap().as_object().unwrap();
        assert_eq!(data.get("fieldA").unwrap(), "a");
        assert!(!data.contains_key("fieldB"));
        assert_eq!(data.get("fieldC").unwrap(), "c");
    }

    #[test]
    fn filter_nested_response() {
        let tree = parse_and_collect("query Q { user { name email @private } }");
        let response = serde_json::json!({
            "data": { "user": { "name": "Alice", "email": "alice@test.com" } }
        });
        let filtered = filter_private_fields(&response, &tree);
        let user = filtered
            .get("data")
            .unwrap()
            .get("user")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(user.get("name").unwrap(), "Alice");
        assert!(!user.contains_key("email"));
    }

    #[test]
    fn filter_array_response() {
        let tree = parse_and_collect("query Q { users { name email @private } }");
        let response = serde_json::json!({
            "data": {
                "users": [
                    { "name": "Alice", "email": "a@test.com" },
                    { "name": "Bob", "email": "b@test.com" }
                ]
            }
        });
        let filtered = filter_private_fields(&response, &tree);
        let users = filtered
            .get("data")
            .unwrap()
            .get("users")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(users.len(), 2);
        assert!(!users[0].as_object().unwrap().contains_key("email"));
        assert!(!users[1].as_object().unwrap().contains_key("email"));
    }

    #[test]
    fn filter_preserves_errors() {
        let tree = parse_and_collect("query Q { fieldA fieldB @private }");
        let response = serde_json::json!({
            "data": { "fieldA": "a", "fieldB": "b" },
            "errors": [{ "message": "something went wrong" }]
        });
        let filtered = filter_private_fields(&response, &tree);
        assert!(filtered.get("errors").is_some());
    }

    #[test]
    fn filter_no_private_fields_returns_same() {
        let tree = parse_and_collect("query Q { fieldA fieldB }");
        let response = serde_json::json!({
            "data": { "fieldA": "a", "fieldB": "b" }
        });
        let filtered = filter_private_fields(&response, &tree);
        assert_eq!(filtered, response);
    }

    #[test]
    fn process_private_directives_includes_fragment_definitions() {
        let query = "query Q { ...F } fragment F on Query { name email @private }";
        let (stripped, tree) = process_private_directives(query).unwrap();
        assert!(tree.children.get("email").unwrap().is_private);
        assert!(
            stripped.contains("fragment F"),
            "stripped query should include fragment definitions, got: {stripped}"
        );
        assert!(!stripped.contains("@private"));
    }
}
