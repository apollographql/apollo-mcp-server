use apollo_compiler::schema::{ExtendedType, Type};
use regex::Regex;
use std::sync::OnceLock;

pub trait MinifyExt {
    /// Serialize in minified form
    fn minify(&self) -> String;
}

impl MinifyExt for ExtendedType {
    fn minify(&self) -> String {
        match self {
            ExtendedType::Scalar(scalar_type) => {
                shorten_scalar_names(scalar_type.name.as_str()).to_string()
            }
            ExtendedType::Object(object_type) => {
                let mut fields = String::new();
                for (field_name, field) in object_type.fields.iter() {
                    if let Some(desc) = field.description.as_ref() {
                        fields.push_str(&format!("\"{}\"", normalize_description(desc)));
                    }
                    fields.push_str(field_name.as_str());
                    if !field.arguments.is_empty() {
                        fields.push('(');
                        fields.push_str(
                            field
                                .arguments
                                .iter()
                                .map(|arg| {
                                    if let Some(desc) = arg.description.as_ref() {
                                        format!(
                                            "\"{}\"{}:{}",
                                            normalize_description(desc),
                                            arg.name.as_str(),
                                            type_name(&arg.ty)
                                        )
                                    } else {
                                        type_name(&arg.ty)
                                    }
                                })
                                .collect::<Vec<String>>()
                                .join(",")
                                .as_str(),
                        );
                        fields.push(')');
                    }
                    fields.push(':');
                    fields.push_str(&type_name(&field.ty));
                    fields.push(',');
                }
                fields.pop();
                let type_name = if let Some(desc) = object_type.description.as_ref() {
                    format!("\"{}\"{}", normalize_description(desc), object_type.name)
                } else {
                    object_type.name.to_string()
                };
                let interfaces = object_type
                    .implements_interfaces
                    .iter()
                    .map(|interface| interface.as_str())
                    .collect::<Vec<&str>>()
                    .join(",");
                if interfaces.is_empty() {
                    format!("T:{type_name}:{fields}")
                } else {
                    format!("T:{type_name}<{interfaces}>:{fields}")
                }
            }
            ExtendedType::Interface(interface_type) => {
                let mut fields = String::new();
                for (field_name, field) in interface_type.fields.iter() {
                    if let Some(desc) = field.description.as_ref() {
                        fields.push_str(&format!("\"{}\"", normalize_description(desc)));
                    }
                    fields.push_str(field_name.as_str());
                    if !field.arguments.is_empty() {
                        fields.push('(');
                        fields.push_str(
                            field
                                .arguments
                                .iter()
                                .map(|arg| {
                                    if let Some(desc) = arg.description.as_ref() {
                                        format!(
                                            "\"{}\"{}:{}",
                                            normalize_description(desc),
                                            arg.name.as_str(),
                                            type_name(&arg.ty)
                                        )
                                    } else {
                                        type_name(&arg.ty)
                                    }
                                })
                                .collect::<Vec<String>>()
                                .join(",")
                                .as_str(),
                        );
                        fields.push(')');
                    }
                    fields.push(':');
                    fields.push_str(&type_name(&field.ty));
                    fields.push(',');
                }
                fields.pop();
                let type_name = if let Some(desc) = interface_type.description.as_ref() {
                    format!("\"{}\"{}", normalize_description(desc), interface_type.name)
                } else {
                    interface_type.name.to_string()
                };
                format!("F:{type_name}:{fields}")
            }
            ExtendedType::Union(union_type) => {
                let mut types = String::new();
                for type_name in union_type.members.iter() {
                    types.push_str(type_name.as_str());
                    types.push(',');
                }
                types.pop();
                let type_name = if let Some(desc) = union_type.description.as_ref() {
                    format!("\"{}\"{}", normalize_description(desc), union_type.name)
                } else {
                    union_type.name.to_string()
                };
                format!("U:{type_name}:{types}")
            }
            ExtendedType::Enum(enum_type) => {
                let mut values = String::new();
                for value in enum_type.values.keys() {
                    values.push_str(value.as_str());
                    values.push(',');
                }
                values.pop();
                let type_name = if let Some(desc) = enum_type.description.as_ref() {
                    format!("\"{}\"{}", normalize_description(desc), enum_type.name)
                } else {
                    enum_type.name.to_string()
                };
                format!("E:{type_name}:{values}")
            }
            ExtendedType::InputObject(input_object_type) => {
                let mut fields = String::new();
                for (field_name, field) in input_object_type.fields.iter() {
                    if let Some(desc) = field.description.as_ref() {
                        fields.push_str(&format!("\"{}\"", normalize_description(desc)));
                    }
                    fields.push_str(field_name.as_str());
                    fields.push(':');
                    fields.push_str(&type_name(&field.ty));
                    fields.push(',');
                }
                fields.pop();
                let type_name = if let Some(desc) = input_object_type.description.as_ref() {
                    format!(
                        "\"{}\"{}",
                        normalize_description(desc),
                        input_object_type.name
                    )
                } else {
                    input_object_type.name.to_string()
                };
                format!("I:{type_name}:{fields}")
            }
        }
    }
}

fn type_name(ty: &Type) -> String {
    let name = shorten_scalar_names(ty.inner_named_type().as_str());
    if ty.is_list() {
        format!("[{name}]")
    } else if ty.is_non_null() {
        format!("{name}!")
    } else {
        name.to_string()
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
