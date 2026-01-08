//! Environment variable expansion for configuration files.
//!
//! Supports `${env.VAR_NAME}` and `${env.VAR_NAME:-default}` syntax.
//! Use `$${env.VAR}` to escape. See config-file.mdx for full documentation.
//!
//! ## Default value behavior
//!
//! - `${env.VAR:-default}` uses `default` if `VAR` is **unset or empty**
//! - This matches bash and Apollo Router behavior
//! - Default values are literal text — nested `${env.X}` references are NOT expanded
//! - Default values may contain `}` if braces are balanced: `${env.VAR:-{"key":"val"}}`

use std::env;
use std::iter::Peekable;
use std::str::Chars;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum EnvExpansionError {
    #[error("undefined environment variable '{name}' referenced in configuration")]
    UndefinedVariable { name: String },

    #[error("environment variable '{name}' contains non-UTF8 data")]
    NonUnicodeValue { name: String },
}

/// Expand all `${env.VAR_NAME}` references in the given content.
pub fn expand_env_vars(content: &str) -> Result<String, EnvExpansionError> {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            result.push(c);
            continue;
        }

        // Escape sequence: $$ followed by { becomes literal ${
        // Standalone $$ is preserved literally
        if chars.peek() == Some(&'$') {
            chars.next(); // consume second $
            if chars.peek() == Some(&'{') {
                result.push('$'); // convert $${ -> ${
            } else {
                result.push_str("$$"); // convert$$<other> -> $$
            }
            continue;
        }

        if chars.peek() != Some(&'{') {
            result.push('$');
            continue;
        }

        expand_placeholder(&mut chars, &mut result)?;
    }

    Ok(result)
}

/// Attempts to expand a `${env.VAR_NAME}` or `${env.VAR_NAME:-default}` placeholder.
///
/// Called when we've seen `$` and the next char is `{`. Parses the placeholder
/// body and either expands it (if valid) or outputs it literally.
fn expand_placeholder(
    chars: &mut Peekable<Chars>,
    result: &mut String,
) -> Result<(), EnvExpansionError> {
    chars.next(); // consume '{'
    let body = read_placeholder_body(chars);

    // Must start with "env." to be a valid placeholder
    let Some(without_prefix) = body.strip_prefix("env.") else {
        result.push_str("${");
        result.push_str(&body);
        return Ok(());
    };

    let Some(inner) = without_prefix.strip_suffix('}') else {
        result.push_str("${");
        result.push_str(&body);
        return Ok(());
    };

    // "":-" cannot appear in valid POSIX variable names, so the first
    // occurrence always delimits var_name from default_value.
    let (var_name, default_value) = match inner.split_once(":-") {
        Some((name, default)) => (name, Some(default)),
        None => (inner, None),
    };

    if !is_valid_var_name(var_name) {
        // Invalid variable name, output literally
        result.push_str("${");
        result.push_str(&body);
        return Ok(());
    }

    match env::var(var_name) {
        Ok(value) if !value.is_empty() => {
            result.push_str(&value);
            Ok(())
        }
        Ok(_) | Err(env::VarError::NotPresent) => {
            if let Some(default) = default_value {
                result.push_str(default);
                Ok(())
            } else {
                Err(EnvExpansionError::UndefinedVariable {
                    name: var_name.to_string(),
                })
            }
        }
        Err(env::VarError::NotUnicode(_)) => Err(EnvExpansionError::NonUnicodeValue {
            name: var_name.to_string(),
        }),
    }
}

/// Reads characters until the matching closing `}` is found or EOF.
///
/// Tracks brace depth to handle nested braces in default values, e.g.,
/// `${env.VAR:-{"key":"value"}}` correctly captures the entire default.
fn read_placeholder_body(chars: &mut Peekable<Chars>) -> String {
    let mut body = String::with_capacity(32);
    let mut depth = 1;

    for ch in chars.by_ref() {
        body.push(ch);
        if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
    }
    body
}

/// Validates that a string is a valid environment variable name.
///
/// Valid names start with an ASCII letter or underscore, followed by
/// zero or more ASCII alphanumeric characters or underscores.
fn is_valid_var_name(name: &str) -> bool {
    let mut chars = name.chars();

    let Some(first) = chars.next() else {
        return false;
    };

    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }

    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
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
    fn ignores_invalid_syntax() {
        let test_cases = [
            ("${VAR}", "${VAR}"),
            ("$env.VAR", "$env.VAR"),
            ("${env.}", "${env.}"),
            ("${env.123start}", "${env.123start}"),
            ("${other.VAR}", "${other.VAR}"),
        ];
        for (input, expected) in test_cases {
            let result = expand_env_vars(input).unwrap();
            assert_eq!(result, expected, "should not expand: {}", input);
        }
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
        let result = expand_env_vars(r#"val: ${env.UNDEFINED_VAR_XYZ:-{"key":"value"}}"#).unwrap();
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
    fn invalid_var_name_with_default_outputs_literally() {
        let result = expand_env_vars("val: ${env.123VAR:-default}").unwrap();
        assert_eq!(result, "val: ${env.123VAR:-default}");
    }

    #[test]
    fn empty_var_name_with_default_outputs_literally() {
        let result = expand_env_vars("val: ${env.:-default}").unwrap();
        assert_eq!(result, "val: ${env.:-default}");
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
        assert_eq!(result, "val: has $${env.X} inside");
    }

    #[test]
    fn triple_dollar_sign() {
        let result = expand_env_vars("val: $$$").unwrap();
        assert_eq!(result, "val: $$$");
    }

    #[test]
    fn quadruple_dollar_before_placeholder() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("X", "value");
            let result = expand_env_vars("val: $$$${env.X}").unwrap();
            assert_eq!(result, "val: $$${env.X}");
            Ok(())
        });
    }

    #[test]
    fn triple_dollar_before_placeholder() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("X", "value");
            let result = expand_env_vars("val: $$${env.X}").unwrap();
            assert_eq!(result, "val: $$value");
            Ok(())
        });
    }

    #[test]
    fn hyphen_in_var_name_treated_as_invalid() {
        let result = expand_env_vars("val: ${env.FOO-BAR}").unwrap();
        assert_eq!(result, "val: ${env.FOO-BAR}");
    }

    #[test]
    fn nested_placeholder_syntax_in_default_is_literal() {
        let result =
            expand_env_vars("val: ${env.MISSING_XYZ:-prefix ${env.OTHER} suffix}").unwrap();
        assert_eq!(result, "val: prefix ${env.OTHER} suffix");
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
    fn whitespace_around_delimiter_treated_as_literal() {
        // Whitespace makes the var name invalid, so output literally
        let result = expand_env_vars("val: ${env.VAR :- default}").unwrap();
        assert_eq!(result, "val: ${env.VAR :- default}");
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
