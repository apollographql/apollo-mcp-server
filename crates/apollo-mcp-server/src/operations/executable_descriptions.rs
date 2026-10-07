//! Descriptions on executable definitions, introduced in the GraphQL September 2025 spec

use apollo_compiler::Node;
use apollo_compiler::ast::{Definition, Document};

/// Returns a description's text, treating a blank description as missing so that the next
/// description source applies.
pub(crate) fn non_blank_description(description: Option<&Node<str>>) -> Option<String> {
    description
        .map(|description| description.trim())
        .filter(|description| !description.is_empty())
        .map(str::to_string)
}

/// Removes descriptions from every operation, variable, and fragment in the document, and
/// returns whether it removed any.
///
/// Servers whose parsers predate the September 2025 spec, including the Apollo Router,
/// reject documents that describe executable definitions.
pub(crate) fn strip_executable_descriptions(document: &mut Document) -> bool {
    let mut stripped = false;
    for definition in &mut document.definitions {
        match definition {
            Definition::OperationDefinition(operation)
                if operation.description.is_some()
                    || operation
                        .variables
                        .iter()
                        .any(|variable| variable.description.is_some()) =>
            {
                let operation = Node::make_mut(operation);
                operation.description = None;
                for variable in &mut operation.variables {
                    if variable.description.is_some() {
                        Node::make_mut(variable).description = None;
                    }
                }
                stripped = true;
            }
            Definition::FragmentDefinition(fragment) if fragment.description.is_some() => {
                Node::make_mut(fragment).description = None;
                stripped = true;
            }
            _ => {}
        }
    }
    stripped
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Node;
    use apollo_compiler::ast::Document;

    use super::{non_blank_description, strip_executable_descriptions};

    fn parse(source: &str) -> Document {
        Document::parse(source, "operation.graphql").unwrap()
    }

    #[test]
    fn strip_executable_descriptions_reports_nothing_stripped_without_descriptions() {
        let mut document = parse("query Q($id: ID) { id ...F } fragment F on Query { id }");

        assert!(!strip_executable_descriptions(&mut document));
    }

    #[test]
    fn strip_executable_descriptions_reports_stripped_operation_description() {
        let mut document = parse(r#""Get it" query Q { id }"#);

        assert!(strip_executable_descriptions(&mut document));
    }

    #[test]
    fn strip_executable_descriptions_reports_stripped_variable_description() {
        let mut document = parse(r#"query Q("The ID" $id: ID) { id }"#);

        assert!(strip_executable_descriptions(&mut document));
    }

    #[test]
    fn strip_executable_descriptions_reports_stripped_fragment_description() {
        let mut document = parse(r#"query Q { ...F } "Shared fields" fragment F on Query { id }"#);

        assert!(strip_executable_descriptions(&mut document));
    }

    #[test]
    fn strip_executable_descriptions_removes_every_description() {
        let mut document = parse(
            r#""Get it" query Q("The ID" $id: ID) { ...F } "Shared fields" fragment F on Query { id }"#,
        );
        strip_executable_descriptions(&mut document);

        insta::assert_snapshot!(
            document.serialize().no_indent(),
            @"query Q($id: ID) { ...F } fragment F on Query { id }"
        );
    }

    #[test]
    fn non_blank_description_is_none_without_description() {
        assert_eq!(non_blank_description(None), None);
    }

    #[test]
    fn non_blank_description_treats_blank_description_as_missing() {
        assert_eq!(non_blank_description(Some(&Node::new_str("   "))), None);
    }

    #[test]
    fn non_blank_description_trims_surrounding_whitespace() {
        assert_eq!(
            non_blank_description(Some(&Node::new_str("  Look up a thing  "))),
            Some("Look up a thing".to_string())
        );
    }
}
