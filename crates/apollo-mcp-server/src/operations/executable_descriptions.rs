//! Descriptions on executable definitions, introduced in the GraphQL September 2025 spec

use apollo_compiler::Node;
use apollo_compiler::ast::{Definition, Document};

/// Returns whether any operation, variable, or fragment in the document has a description.
pub(crate) fn has_executable_descriptions(document: &Document) -> bool {
    document
        .definitions
        .iter()
        .any(|definition| match definition {
            Definition::OperationDefinition(operation) => {
                operation.description.is_some()
                    || operation
                        .variables
                        .iter()
                        .any(|variable| variable.description.is_some())
            }
            Definition::FragmentDefinition(fragment) => fragment.description.is_some(),
            _ => false,
        })
}

/// Removes descriptions from every operation, variable, and fragment in the document.
///
/// Servers whose parsers predate the September 2025 spec, including the Apollo Router,
/// reject documents that describe executable definitions.
pub(crate) fn strip_executable_descriptions(mut document: Document) -> Document {
    for definition in &mut document.definitions {
        match definition {
            Definition::OperationDefinition(operation) => {
                let operation = Node::make_mut(operation);
                operation.description = None;
                for variable in &mut operation.variables {
                    Node::make_mut(variable).description = None;
                }
            }
            Definition::FragmentDefinition(fragment) => {
                Node::make_mut(fragment).description = None;
            }
            _ => {}
        }
    }
    document
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ast::Document;

    use super::{has_executable_descriptions, strip_executable_descriptions};

    fn parse(source: &str) -> Document {
        Document::parse(source, "operation.graphql").unwrap()
    }

    #[test]
    fn has_executable_descriptions_is_false_without_descriptions() {
        let document = parse("query Q($id: ID) { id ...F } fragment F on Query { id }");

        assert!(!has_executable_descriptions(&document));
    }

    #[test]
    fn has_executable_descriptions_detects_operation_description() {
        let document = parse(r#""Get it" query Q { id }"#);

        assert!(has_executable_descriptions(&document));
    }

    #[test]
    fn has_executable_descriptions_detects_variable_description() {
        let document = parse(r#"query Q("The ID" $id: ID) { id }"#);

        assert!(has_executable_descriptions(&document));
    }

    #[test]
    fn has_executable_descriptions_detects_fragment_description() {
        let document = parse(r#"query Q { ...F } "Shared fields" fragment F on Query { id }"#);

        assert!(has_executable_descriptions(&document));
    }

    #[test]
    fn strip_executable_descriptions_removes_every_description() {
        let document = parse(
            r#""Get it" query Q("The ID" $id: ID) { ...F } "Shared fields" fragment F on Query { id }"#,
        );

        insta::assert_snapshot!(
            strip_executable_descriptions(document).serialize().no_indent(),
            @"query Q($id: ID) { ...F } fragment F on Query { id }"
        );
    }
}
