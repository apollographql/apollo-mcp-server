/// Macro to generate a JSON schema from a type
#[macro_export]
macro_rules! schema_from_type {
    ($type:ty) => {{
        match serde_json::to_value(schemars::schema_for!($type)) {
            Ok(Value::Object(schema)) => schema,
            _ => panic!("Failed to generate schema for {}", stringify!($type)),
        }
    }};
}
