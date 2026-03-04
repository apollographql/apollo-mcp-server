use apollo_compiler::schema::{ExtendedType, Type};
use regex::Regex;
use std::{collections::HashMap, sync::OnceLock};

pub trait MinifyExt {
    /// Serialize in minified form
    fn minify(&self) -> String;
}

impl MinifyExt for ExtendedType {
    fn minify(&self) -> String {
        match self {
            ExtendedType::Scalar(scalar_type) => minify_scalar(scalar_type),
            ExtendedType::Object(object_type) => minify_object(object_type),
            ExtendedType::Interface(interface_type) => minify_interface(interface_type),
            ExtendedType::Union(union_type) => minify_union(union_type),
            ExtendedType::Enum(enum_type) => minify_enum(enum_type),
            ExtendedType::InputObject(input_object_type) => minify_input_object(input_object_type),
        }
    }
}

fn minify_scalar(scalar_type: &apollo_compiler::schema::ScalarType) -> String {
    shorten_scalar_names(scalar_type.name.as_str()).to_string()
}

fn minify_object(object_type: &apollo_compiler::schema::ObjectType) -> String {
    let fields = minify_fields(&object_type.fields);
    let type_name = format_type_name_with_description(&object_type.name, &object_type.description);
    let interfaces = format_interfaces(&object_type.implements_interfaces);

    if interfaces.is_empty() {
        format!("T:{type_name}:{fields}")
    } else {
        format!("T:{type_name}<{interfaces}>:{fields}")
    }
}

fn minify_interface(interface_type: &apollo_compiler::schema::InterfaceType) -> String {
    let fields = minify_fields(&interface_type.fields);
    let type_name =
        format_type_name_with_description(&interface_type.name, &interface_type.description);
    format!("F:{type_name}:{fields}")
}

fn minify_union(union_type: &apollo_compiler::schema::UnionType) -> String {
    let member_types = union_type
        .members
        .iter()
        .map(|member| member.as_str())
        .collect::<Vec<&str>>()
        .join(",");
    let type_name = format_type_name_with_description(&union_type.name, &union_type.description);
    format!("U:{type_name}:{member_types}")
}

fn minify_enum(enum_type: &apollo_compiler::schema::EnumType) -> String {
    let values = enum_type
        .values
        .keys()
        .map(|value| value.as_str())
        .collect::<Vec<&str>>()
        .join(",");
    let type_name = format_type_name_with_description(&enum_type.name, &enum_type.description);
    format!("E:{type_name}:{values}")
}

fn minify_input_object(input_object_type: &apollo_compiler::schema::InputObjectType) -> String {
    let fields = minify_input_fields(&input_object_type.fields);
    let type_name =
        format_type_name_with_description(&input_object_type.name, &input_object_type.description);
    format!("I:{type_name}:{fields}")
}

// We should only minify directives that assist the LLM in understanding the schema. This included @deprecated
fn minify_directives(directives: &apollo_compiler::ast::DirectiveList) -> String {
    let mut result = String::new();

    static DIRECTIVES_TO_MINIFY: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    let directives_to_minify =
        DIRECTIVES_TO_MINIFY.get_or_init(|| HashMap::from([("deprecated", "D")]));

    for directive in directives.iter() {
        if let Some(minified_name) = directives_to_minify.get(directive.name.as_str()) {
            // Since we're only handling @deprecated right now we can just add the reason and minify it.
            // We should handle this more generically in the future.
            if !directive.arguments.is_empty()
                && let Some(reason) = directive
                    .arguments
                    .iter()
                    .find(|a| a.name == "reason")
                    .and_then(|a| a.value.as_str())
            {
                result.push_str(&format!(
                    "@{}(\"{}\")",
                    minified_name,
                    normalize_description(reason)
                ));
            } else {
                result.push_str(&format!("@{}", minified_name));
            }
        }
    }
    result
}

fn minify_fields(
    fields: &apollo_compiler::collections::IndexMap<
        apollo_compiler::Name,
        apollo_compiler::schema::Component<apollo_compiler::ast::FieldDefinition>,
    >,
) -> String {
    let mut result = String::new();

    for (field_name, field) in fields.iter() {
        // Add description if present
        if let Some(desc) = field.description.as_ref() {
            result.push_str(&format!("\"{}\"", normalize_description(desc)));
        }

        // Add field name
        result.push_str(field_name.as_str());

        // Add arguments if present
        if !field.arguments.is_empty() {
            result.push('(');
            result.push_str(&minify_arguments(&field.arguments));
            result.push(')');
        }

        // Add field type
        result.push(':');
        result.push_str(&type_name(&field.ty));
        result.push_str(&minify_directives(&field.directives));

        result.push(',');
    }

    // Remove trailing comma
    if !result.is_empty() {
        result.pop();
    }

    result
}

fn minify_input_fields(
    fields: &apollo_compiler::collections::IndexMap<
        apollo_compiler::Name,
        apollo_compiler::schema::Component<apollo_compiler::ast::InputValueDefinition>,
    >,
) -> String {
    let mut result = String::new();

    for (field_name, field) in fields.iter() {
        // Add description if present
        if let Some(desc) = field.description.as_ref() {
            result.push_str(&format!("\"{}\"", normalize_description(desc)));
        }

        // Add field name and type
        result.push_str(field_name.as_str());
        result.push(':');
        result.push_str(&type_name(&field.ty));
        result.push_str(&minify_directives(&field.directives));
        result.push(',');
    }

    // Remove trailing comma
    if !result.is_empty() {
        result.pop();
    }

    result
}

fn minify_arguments(
    arguments: &[apollo_compiler::Node<apollo_compiler::ast::InputValueDefinition>],
) -> String {
    arguments
        .iter()
        .map(|arg| {
            if let Some(desc) = arg.description.as_ref() {
                format!(
                    "\"{}\"{}:{}{}",
                    normalize_description(desc),
                    arg.name.as_str(),
                    type_name(&arg.ty),
                    minify_directives(&arg.directives)
                )
            } else {
                format!(
                    "{}:{}{}",
                    arg.name.as_str(),
                    type_name(&arg.ty),
                    minify_directives(&arg.directives)
                )
            }
        })
        .collect::<Vec<String>>()
        .join(",")
}

fn format_type_name_with_description(
    name: &apollo_compiler::Name,
    description: &Option<apollo_compiler::Node<str>>,
) -> String {
    if let Some(desc) = description.as_ref() {
        format!("\"{}\"{}", normalize_description(desc), name)
    } else {
        name.to_string()
    }
}

fn format_interfaces(
    interfaces: &apollo_compiler::collections::IndexSet<apollo_compiler::schema::ComponentName>,
) -> String {
    interfaces
        .iter()
        .map(|interface| interface.as_str())
        .collect::<Vec<&str>>()
        .join(",")
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::List(inner) => format!("[{}]", type_name(inner)),
        Type::NonNullList(inner) => format!("[{}]!", type_name(inner)),
        #[allow(clippy::useless_format)]
        Type::Named(name) => format!("{}", shorten_scalar_names(name.as_str())),
        Type::NonNullNamed(name) => format!("{}!", shorten_scalar_names(name.as_str())),
    }
}

fn shorten_scalar_names(name: &str) -> &str {
    match name {
        "String" => "s",
        "Int" => "i",
        "Float" => "f",
        "Boolean" => "b",
        "ID" => "d",
        _ => name,
    }
}

/// Normalize description formatting
#[allow(clippy::expect_used)]
fn normalize_description(desc: &str) -> String {
    // LLMs can typically process descriptions just fine without whitespace
    static WHITESPACE_PATTERN: OnceLock<Regex> = OnceLock::new();
    let re = WHITESPACE_PATTERN.get_or_init(|| Regex::new(r"\s+").expect("regex pattern compiles"));
    re.replace_all(desc, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCHEMA: &str = include_str!("tools/testdata/schema.graphql");

    #[test]
    fn minify_schema() {
        let schema = apollo_compiler::schema::Schema::parse(TEST_SCHEMA, "schema.graphql")
            .expect("Failed to parse schema")
            .validate()
            .expect("Failed to validate schema");

        let minified = schema
            .types
            .iter()
            .map(|(_, type_)| format!("{}: {}", type_.name().as_str(), type_.minify()))
            .collect::<Vec<String>>()
            .join("\n");

        insta::assert_snapshot!(minified);
    }
}
