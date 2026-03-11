/// Macro to generate a JSON schema from a type
#[macro_export]
macro_rules! schema_from_type {
    ($type:ty) => {{
        // Use Draft-07 for compatibility with MCP clients like VSCode/Copilot that don't support newer drafts.
        // See: https://github.com/microsoft/vscode/issues/251315
        let settings = schemars::generate::SchemaSettings::draft07();
        let generator = settings.into_generator();
        let schema = generator.into_root_schema_for::<$type>();
        match serde_json::to_value(schema) {
            Ok(Value::Object(schema)) => schema,
            _ => panic!("Failed to generate schema for {}", stringify!($type)),
        }
    }};
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(JsonSchema, Deserialize)]
    struct TestInput {
        #[allow(dead_code)]
        field: String,
    }

    #[test]
    fn schema_from_type() {
        let schema = schema_from_type!(TestInput);

        assert_eq!(
            serde_json::to_value(&schema).unwrap(),
            serde_json::json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "title": "TestInput",
                "type": "object",
                "properties": {
                    "field": {
                        "type": "string"
                    }
                },
                "required": ["field"]
            })
        );
    }
}
