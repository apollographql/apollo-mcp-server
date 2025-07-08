use apollo_compiler::schema::{ExtendedType, Type};

pub trait Minify {
    fn minify(&self) -> String;
}

// TODO: descriptions
impl Minify for ExtendedType {
    fn minify(&self) -> String {
        match self {
            ExtendedType::Scalar(scalar_type) => match scalar_type.name.as_str() {
                "String" => "s",
                "Int" => "i",
                "Float" => "f",
                "Boolean" => "b",
                "ID" => "d",
                _ => scalar_type.name.as_str(),
            }
            .to_string(),
            ExtendedType::Object(object_type) => {
                let mut fields = String::new();
                for (field_name, field) in object_type.fields.iter() {
                    fields.push_str(field_name.as_str());
                    if !field.arguments.is_empty() {
                        fields.push('(');
                        fields.push_str(
                            field
                                .arguments
                                .iter()
                                .map(|arg| type_name(&arg.ty))
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
                format!("T:{}:{}", object_type.name, fields)
            }
            ExtendedType::Interface(interface_type) => {
                let mut fields = String::new();
                for (field_name, field) in interface_type.fields.iter() {
                    fields.push_str(field_name.as_str());
                    if !field.arguments.is_empty() {
                        fields.push('(');
                        fields.push_str(
                            field
                                .arguments
                                .iter()
                                .map(|arg| type_name(&arg.ty))
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
                format!("F:{}:{}", interface_type.name, fields)
            }
            ExtendedType::Union(union_type) => {
                let mut types = String::new();
                for type_name in union_type.members.iter() {
                    types.push_str(type_name.as_str());
                    types.push(',');
                }
                types.pop();
                format!("U:{}:{}", union_type.name, types)
            }
            ExtendedType::Enum(enum_type) => {
                let mut values = String::new();
                for value in enum_type.values.keys() {
                    values.push_str(value.as_str());
                    values.push(',');
                }
                values.pop();
                format!("E:{}:{}", enum_type.name, values)
            }
            ExtendedType::InputObject(input_object_type) => {
                let mut fields = String::new();
                for (field_name, field) in input_object_type.fields.iter() {
                    fields.push_str(field_name.as_str());
                    fields.push(':');
                    fields.push_str(&type_name(&field.ty));
                    fields.push(',');
                }
                fields.pop();
                format!("I:{}:{}", input_object_type.name, fields)
            }
        }
    }
}

fn type_name(ty: &Type) -> String {
    let name = ty.inner_named_type().as_str();
    if ty.is_list() {
        format!("[{name}]")
    } else if ty.is_non_null() {
        format!("{name}!")
    } else {
        name.to_string()
    }
}
