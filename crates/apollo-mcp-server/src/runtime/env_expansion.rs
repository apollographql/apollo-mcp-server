//! Environment variable expansion for configuration files.
//!
//! Supports `${env.VAR_NAME}` syntax. Use `$${env.VAR}` to escape.
//! See config-file.mdx for full documentation.

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

/// Attempts to expand a `${env.VAR_NAME}` placeholder.
///
/// Called when we've seen `$` and the next char is `{`. Parses the placeholder
/// body and either expands it (if valid) or outputs it literally.
fn expand_placeholder(
    chars: &mut Peekable<Chars>,
    result: &mut String,
) -> Result<(), EnvExpansionError> {
    chars.next(); // consume '{'
    let body = read_placeholder_body(chars);

    // Extract var name: must be "env.<name>}" format
    let Some(var_name) = body
        .strip_prefix("env.")
        .and_then(|s| s.strip_suffix('}'))
    else {
        result.push_str("${");
        result.push_str(&body);
        return Ok(());
    };

    if !is_valid_var_name(var_name) {        // Invalid variable name, output literally
        result.push_str("${");
        result.push_str(&body);
        return Ok(());
    }

    match env::var(var_name) {
        Ok(value) => {
            result.push_str(&value);
            Ok(())
        }
        Err(env::VarError::NotPresent) => Err(EnvExpansionError::UndefinedVariable {
            name: var_name.to_string(),
        }),
        Err(env::VarError::NotUnicode(_)) => Err(EnvExpansionError::NonUnicodeValue {
            name: var_name.to_string(),
        }),
    }
}

/// Reads characters until a closing `}` is found or until we reach the end of input.
fn read_placeholder_body(chars: &mut Peekable<Chars>) -> String {
    let mut body = String::new();
    for ch in chars.by_ref() {
        body.push(ch);
        if ch == '}' {
            break;
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
    fn handles_empty_env_var_value() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("EMPTY_VAR", "");
            let result = expand_env_vars("val: ${env.EMPTY_VAR}").unwrap();
            assert_eq!(result, "val: ");
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
}
