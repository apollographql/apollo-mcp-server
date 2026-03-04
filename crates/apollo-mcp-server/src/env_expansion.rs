//! Environment variable expansion for configuration files.
//!
//! Supports `${env.VAR_NAME}` and `${env.VAR_NAME:-default}` syntax.

use serde_yaml::Value;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum EnvExpansionError {
    #[error("undefined environment variable '{name}' referenced in configuration")]
    UndefinedVariable { name: String },

    #[error("environment variable '{name}' contains non-UTF8 data")]
    NonUnicodeValue { name: String },

    #[error("failed to parse YAML: {0}")]
    YamlParse(String),

    #[error("failed to serialize YAML: {0}")]
    YamlSerialize(String),
}

/// Expand environment variables in YAML content with type coercion.
///
/// This is the main entry point. It parses the YAML, walks the AST to expand
/// environment variables, coerces types, and returns the expanded YAML string.
pub fn expand_yaml(yaml_content: &str) -> Result<String, EnvExpansionError> {
    // Parse YAML to AST
    let mut value: Value = serde_yaml::from_str(yaml_content)
        .map_err(|e| EnvExpansionError::YamlParse(e.to_string()))?;

    // Walk and expand environment variables
    expand_value(&mut value)?;

    // Serialize back to YAML string
    serde_yaml::to_string(&value).map_err(|e| EnvExpansionError::YamlSerialize(e.to_string()))
}

/// Recursively walk the YAML AST and expand environment variables in string nodes.
fn expand_value(value: &mut Value) -> Result<(), EnvExpansionError> {
    match value {
        Value::String(s) => {
            // Only expand if the string contains potential placeholders
            if s.contains("${") || s.contains("$$") {
                let expanded = expand_env_vars(s)?;
                // Coerce the expanded string to the appropriate type
                *value = coerce(&expanded);
            }
        }
        Value::Sequence(seq) => {
            for item in seq {
                expand_value(item)?;
            }
        }
        Value::Mapping(map) => {
            for (_, v) in map {
                expand_value(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Coerce an expanded string to its natural YAML type.
///
/// # Examples
///
/// - `"true"` → `Bool(true)`
/// - `"false"` → `Bool(false)`
/// - `"8080"` → `Number(8080)`
/// - `"3.14"` → `Number(3.14)`
/// - `"null"` → `Null`
/// - `"hello"` → `String("hello")`
/// - `"port: 8080"` → `String("port: 8080")` (stays string, not parsed as mapping)
pub fn coerce(s: &str) -> Value {
    match serde_yaml::from_str(s) {
        Ok(Value::Bool(b)) => Value::Bool(b),
        Ok(Value::Number(n)) => Value::Number(n),
        Ok(Value::Null) => Value::Null,
        // Everything else (including Mapping, Sequence, Tagged) stays as String
        // This prevents "key: value" from being parsed as a nested structure
        _ => Value::String(s.to_string()),
    }
}

/// Expand all `${env.VAR_NAME}` references in the string.
pub(super) fn expand_env_vars(content: &str) -> Result<String, EnvExpansionError> {
    shellexpand::env_with_context(content, context_fn)
        .map(|cow| cow.into_owned())
        .map_err(|e| e.cause)
}

fn context_fn(key: &str) -> Result<Option<String>, EnvExpansionError> {
    let Some(var_name) = key.strip_prefix("env.") else {
        return Ok(None);
    };

    match std::env::var(var_name) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Err(EnvExpansionError::UndefinedVariable {
            name: var_name.to_string(),
        }),
        Err(std::env::VarError::NotUnicode(_)) => Err(EnvExpansionError::NonUnicodeValue {
            name: var_name.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod expand_yaml {
        use super::*;

        #[test]
        fn coerces_boolean() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("MY_BOOL", "true");
                let yaml = "enabled: \"${env.MY_BOOL}\"";
                let result = expand_yaml(yaml).unwrap();
                // Should be coerced to boolean, not string "true"
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                assert_eq!(parsed["enabled"], Value::Bool(true));
                Ok(())
            });
        }

        #[test]
        fn coerces_number() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("MY_PORT", "8080");
                let yaml = "port: \"${env.MY_PORT}\"";
                let result = expand_yaml(yaml).unwrap();
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                assert!(parsed["port"].is_number());
                assert_eq!(parsed["port"].as_u64(), Some(8080));
                Ok(())
            });
        }

        #[test]
        fn coerces_null() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("MY_NULL", "null");
                let yaml = "value: \"${env.MY_NULL}\"";
                let result = expand_yaml(yaml).unwrap();
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                assert!(parsed["value"].is_null());
                Ok(())
            });
        }

        #[test]
        fn preserves_string() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("MY_STRING", "hello world");
                let yaml = "name: \"${env.MY_STRING}\"";
                let result = expand_yaml(yaml).unwrap();
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                assert_eq!(parsed["name"].as_str(), Some("hello world"));
                Ok(())
            });
        }

        #[test]
        fn handles_special_chars_safely() {
            // This would break with pre-parse substitution!
            figment::Jail::expect_with(|jail| {
                jail.set_env("MY_VALUE", "key: value with colon");
                let yaml = "description: \"${env.MY_VALUE}\"";
                let result = expand_yaml(yaml).unwrap();
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                // Should be a string, not a nested mapping
                assert_eq!(
                    parsed["description"].as_str(),
                    Some("key: value with colon")
                );
                Ok(())
            });
        }

        #[test]
        fn expands_nested_structures() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("PORT", "3000");
                jail.set_env("ENABLED", "true");
                let yaml = r#"
server:
  port: "${env.PORT}"
  nested:
    enabled: "${env.ENABLED}"
"#;
                let result = expand_yaml(yaml).unwrap();
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                assert_eq!(parsed["server"]["port"].as_u64(), Some(3000));
                assert_eq!(parsed["server"]["nested"]["enabled"], Value::Bool(true));
                Ok(())
            });
        }

        #[test]
        fn expands_arrays() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("VAL1", "first");
                jail.set_env("VAL2", "42");
                let yaml = r#"
items:
  - "${env.VAL1}"
  - "${env.VAL2}"
"#;
                let result = expand_yaml(yaml).unwrap();
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                assert_eq!(parsed["items"][0].as_str(), Some("first"));
                assert_eq!(parsed["items"][1].as_u64(), Some(42));
                Ok(())
            });
        }

        #[test]
        fn unquoted_also_coerces() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("MY_NUM", "123");
                let yaml = "count: ${env.MY_NUM}";
                let result = expand_yaml(yaml).unwrap();
                let parsed: Value = serde_yaml::from_str(&result).unwrap();
                assert_eq!(parsed["count"].as_u64(), Some(123));
                Ok(())
            });
        }
    }

    mod coerce {
        use super::*;

        #[test]
        fn true_to_bool() {
            assert_eq!(coerce("true"), Value::Bool(true));
        }

        #[test]
        fn false_to_bool() {
            assert_eq!(coerce("false"), Value::Bool(false));
        }

        #[test]
        fn integer_to_number() {
            let result = coerce("42");
            assert!(result.is_number());
            assert_eq!(result.as_u64(), Some(42));
        }

        #[test]
        fn float_to_number() {
            let result = coerce("2.5");
            assert!(result.is_number());
            assert!((result.as_f64().unwrap() - 2.5).abs() < 0.001);
        }

        #[test]
        fn null_to_null() {
            assert!(coerce("null").is_null());
        }

        #[test]
        fn mapping_stays_string() {
            let result = coerce("key: value");
            assert_eq!(result, Value::String("key: value".to_string()));
        }

        #[test]
        fn sequence_stays_string() {
            let result = coerce("[1, 2, 3]");
            assert_eq!(result, Value::String("[1, 2, 3]".to_string()));
        }
    }

    mod expand_env_vars {
        use super::*;

        #[test]
        fn expands_single_env_var() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("TEST_VAR", "expanded_value");
                let result = expand_env_vars("endpoint: ${env.TEST_VAR}").unwrap();
                assert_eq!(result, "endpoint: expanded_value");
                Ok(())
            });
        }

        #[test]
        fn expands_multiple_env_vars() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("VAR_ONE", "first");
                jail.set_env("VAR_TWO", "second");
                let result = expand_env_vars("a: ${env.VAR_ONE}, b: ${env.VAR_TWO}").unwrap();
                assert_eq!(result, "a: first, b: second");
                Ok(())
            });
        }

        #[test]
        fn preserves_content_without_env_vars() {
            let input = "endpoint: http://localhost:4000\nport: 8080";
            let result = expand_env_vars(input).unwrap();
            assert_eq!(result, input);
        }

        #[test]
        fn errors_on_undefined_env_var() {
            let result = expand_env_vars("val: ${env._NONEXISTENT_VAR_12345_}");
            assert_eq!(
                result.unwrap_err(),
                EnvExpansionError::UndefinedVariable {
                    name: "_NONEXISTENT_VAR_12345_".into()
                }
            );
        }

        #[test]
        fn leaves_non_env_prefixed_vars_literal() {
            // Variables without "env." prefix are left literal (context returns Ok(None))
            let literal_cases = [
                ("${VAR}", "${VAR}"),
                ("$env.VAR", "$env.VAR"),
                ("${other.VAR}", "${other.VAR}"),
            ];
            for (input, expected) in literal_cases {
                let result = expand_env_vars(input).unwrap();
                assert_eq!(result, expected, "should stay literal: {}", input);
            }
        }

        #[test]
        fn errors_on_empty_var_name() {
            let result = expand_env_vars("${env.}");
            assert!(result.is_err());
        }

        #[test]
        fn allows_numeric_start_var_names() {
            let result = expand_env_vars("${env.123start}");
            assert!(result.is_err());
        }

        #[test]
        fn empty_var_without_default_errors() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("EMPTY_VAR", "");
                let result = expand_env_vars("val: ${env.EMPTY_VAR}");
                assert_eq!(
                    result.unwrap_err(),
                    EnvExpansionError::UndefinedVariable {
                        name: "EMPTY_VAR".into()
                    }
                );
                Ok(())
            });
        }

        #[test]
        fn expands_underscore_prefixed_vars() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("_PRIVATE_VAR", "private");
                let result = expand_env_vars("val: ${env._PRIVATE_VAR}").unwrap();
                assert_eq!(result, "val: private");
                Ok(())
            });
        }

        #[test]
        fn escapes_double_dollar() {
            let result = expand_env_vars("val: $${env.SOMETHING}").unwrap();
            assert_eq!(result, "val: ${env.SOMETHING}");
        }

        #[test]
        fn handles_unclosed_brace() {
            let result = expand_env_vars("val: ${env.VAR").unwrap();
            assert_eq!(result, "val: ${env.VAR");
        }

        #[test]
        fn handles_dollar_at_end() {
            let result = expand_env_vars("val: test$").unwrap();
            assert_eq!(result, "val: test$");
        }

        #[test]
        fn handles_multiple_escapes() {
            let result = expand_env_vars("$${A} and $${B}").unwrap();
            assert_eq!(result, "${A} and ${B}");
        }

        #[test]
        fn handles_yaml_special_chars_in_value() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("SPECIAL_VAR", "value: with colon");
                let result = expand_env_vars("endpoint: ${env.SPECIAL_VAR}").unwrap();
                assert_eq!(result, "endpoint: value: with colon");
                Ok(())
            });
        }

        #[test]
        fn handles_quoted_value_with_special_chars() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("SPECIAL_VAR", "value: with colon");
                let result = expand_env_vars("endpoint: \"${env.SPECIAL_VAR}\"").unwrap();
                assert_eq!(result, "endpoint: \"value: with colon\"");
                Ok(())
            });
        }

        #[test]
        fn does_not_recursively_expand() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("OUTER", "${env.INNER}");
                jail.set_env("INNER", "should_not_appear");
                let result = expand_env_vars("val: ${env.OUTER}").unwrap();
                assert_eq!(result, "val: ${env.INNER}");
                Ok(())
            });
        }

        #[test]
        fn expands_adjacent_vars() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("A", "first");
                jail.set_env("B", "second");
                let result = expand_env_vars("${env.A}${env.B}").unwrap();
                assert_eq!(result, "firstsecond");
                Ok(())
            });
        }

        #[test]
        fn escapes_first_but_expands_second() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("B", "expanded");
                let result = expand_env_vars("$${env.A}${env.B}").unwrap();
                assert_eq!(result, "${env.A}expanded");
                Ok(())
            });
        }

        #[test]
        fn uses_default_when_var_undefined() {
            let result = expand_env_vars("val: ${env.UNDEFINED_VAR_XYZ:-fallback}").unwrap();
            assert_eq!(result, "val: fallback");
        }

        #[test]
        fn ignores_default_when_var_defined() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("DEFINED_VAR", "actual_value");
                let result = expand_env_vars("val: ${env.DEFINED_VAR:-fallback}").unwrap();
                assert_eq!(result, "val: actual_value");
                Ok(())
            });
        }

        #[test]
        fn uses_empty_default() {
            let result = expand_env_vars("val: ${env.UNDEFINED_VAR_XYZ:-}").unwrap();
            assert_eq!(result, "val: ");
        }

        #[test]
        fn default_with_special_chars() {
            let result =
                expand_env_vars("url: ${env.UNDEFINED_VAR_XYZ:-http://localhost:4000}").unwrap();
            assert_eq!(result, "url: http://localhost:4000");
        }

        #[test]
        fn default_preserves_colons_in_value() {
            let result = expand_env_vars("val: ${env.UNDEFINED_VAR_XYZ:-a:b:c}").unwrap();
            assert_eq!(result, "val: a:b:c");
        }

        #[test]
        fn multiple_vars_with_defaults() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("EXISTS", "real");
                let result =
                    expand_env_vars("${env.EXISTS:-x} ${env.MISSING_VAR_XYZ:-default}").unwrap();
                assert_eq!(result, "real default");
                Ok(())
            });
        }

        #[test]
        fn default_with_nested_braces() {
            let result =
                expand_env_vars(r#"val: ${env.UNDEFINED_VAR_XYZ:-{"key":"value"}}"#).unwrap();
            assert_eq!(result, r#"val: {"key":"value"}"#);
        }

        #[test]
        fn default_with_deeply_nested_braces() {
            let result = expand_env_vars("val: ${env.UNDEFINED_VAR_XYZ:-{a:{b:{c:1}}}}").unwrap();
            assert_eq!(result, "val: {a:{b:{c:1}}}");
        }

        #[test]
        fn empty_var_uses_default() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("EMPTY_VAR", "");
                let result = expand_env_vars("val: ${env.EMPTY_VAR:-fallback}").unwrap();
                assert_eq!(result, "val: fallback");
                Ok(())
            });
        }

        #[test]
        fn numeric_var_name_with_default_uses_default() {
            let result = expand_env_vars("val: ${env.123VAR:-default}").unwrap();
            assert_eq!(result, "val: default");
        }

        #[test]
        fn empty_var_name_with_default_uses_default() {
            let result = expand_env_vars("val: ${env.:-default}").unwrap();
            assert_eq!(result, "val: default");
        }

        #[test]
        fn default_after_nested_braces_continues_parsing() {
            let result = expand_env_vars("${env.UNDEFINED_VAR_XYZ:-{}} more text").unwrap();
            assert_eq!(result, "{} more text");
        }

        #[test]
        fn unbalanced_braces_in_default_outputs_literal() {
            let result = expand_env_vars("val: ${env.VAR:-{unclosed").unwrap();
            assert_eq!(result, "val: ${env.VAR:-{unclosed");
        }

        #[test]
        fn escape_in_default_not_processed() {
            let result =
                expand_env_vars("val: ${env.UNDEFINED_VAR_XYZ:-has $${env.X} inside}").unwrap();
            assert_eq!(result, "val: has $${env.X inside}");
        }

        #[test]
        fn triple_dollar_sign() {
            // $$ -> $, then remaining $ stays literal
            // So $$$ -> $ + $ = $$
            let result = expand_env_vars("val: $$$").unwrap();
            assert_eq!(result, "val: $$");
        }

        #[test]
        fn quadruple_dollar_before_placeholder() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("X", "value");
                // $$$$ = $$ + $$ = $ + $ = $$, then {env.X} is literal (no $ before it)
                let result = expand_env_vars("val: $$$${env.X}").unwrap();
                assert_eq!(result, "val: $${env.X}");
                Ok(())
            });
        }

        #[test]
        fn triple_dollar_before_placeholder() {
            figment::Jail::expect_with(|jail| {
                jail.set_env("X", "value");
                // $$$ = $$ + $ = $ + ${env.X} expanded = $value
                let result = expand_env_vars("val: $$${env.X}").unwrap();
                assert_eq!(result, "val: $value");
                Ok(())
            });
        }

        #[test]
        fn hyphen_in_var_name_errors_if_undefined() {
            let result = expand_env_vars("val: ${env.FOO-BAR}");
            assert!(result.is_err());
        }

        #[test]
        fn nested_placeholder_syntax_in_default_is_literal() {
            let result =
                expand_env_vars("val: ${env.MISSING_XYZ:-prefix ${env.OTHER} suffix}").unwrap();
            assert_eq!(result, "val: prefix ${env.OTHER suffix}");
        }

        #[test]
        fn default_value_containing_colon_hyphen() {
            let result = expand_env_vars("val: ${env.MISSING_XYZ:-a:-b:-c}").unwrap();
            assert_eq!(result, "val: a:-b:-c");
        }

        #[test]
        fn empty_input() {
            assert_eq!(expand_env_vars("").unwrap(), "");
        }

        #[test]
        fn standalone_dollar() {
            assert_eq!(expand_env_vars("$").unwrap(), "$");
        }

        #[test]
        fn whitespace_around_delimiter_uses_default() {
            let result = expand_env_vars("val: ${env.VAR :- default}").unwrap();
            assert_eq!(result, "val:  default");
        }

        #[test]
        fn quoted_brace_in_default_causes_early_termination() {
            // Known limitation: quotes don't prevent brace matching
            // The } inside quotes terminates the placeholder early
            let result = expand_env_vars(r#"val: ${env.MISSING_XYZ:-"}"}"#).unwrap();
            // Placeholder ends at first }, default is just ", remaining "} is literal
            assert_eq!(result, r#"val: ""}"#);
        }
    }
}
