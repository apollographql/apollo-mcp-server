use rhai::plugin::*;
use rhai::{Engine, Module};
use rhai::{export_module, exported_module};

pub(crate) struct RhaiRegex {}

impl RhaiRegex {
    pub(crate) fn register(engine: &mut Engine) {
        engine.register_static_module("Regex", exported_module!(rhai_regex_module).into());
    }
}

// Rhai's #[export_module] macro generates code that uses unwrap internally
#[allow(clippy::unwrap_used)]
#[export_module]
mod rhai_regex_module {
    use rhai::Array;
    use rhai::Dynamic;
    use rhai::ImmutableString;

    #[rhai_fn(return_raw)]
    pub(crate) fn is_match(
        string: ImmutableString,
        pattern: ImmutableString,
    ) -> Result<bool, Box<EvalAltResult>> {
        let re = regex::Regex::new(pattern.as_str()).map_err(|e| e.to_string())?;
        Ok(re.is_match(string.as_str()))
    }

    #[rhai_fn(return_raw)]
    pub(crate) fn replace(
        string: ImmutableString,
        pattern: ImmutableString,
        replace_with: ImmutableString,
    ) -> Result<String, Box<EvalAltResult>> {
        let re = regex::Regex::new(pattern.as_str()).map_err(|e| e.to_string())?;
        Ok(re
            .replace_all(string.as_str(), replace_with.as_str())
            .into_owned())
    }

    #[rhai_fn(return_raw)]
    pub(crate) fn matches(
        string: ImmutableString,
        pattern: ImmutableString,
    ) -> Result<Array, Box<EvalAltResult>> {
        let re = regex::Regex::new(pattern.as_str()).map_err(|e| e.to_string())?;
        let matches: Array = re
            .find_iter(string.as_str())
            .map(|m| Dynamic::from(m.as_str().to_string()))
            .collect();
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use rhai::{Engine, EvalAltResult, FuncArgs, Scope};

    use crate::rhai::functions::RhaiRegex;

    fn run_rhai_script<T: Clone + Send + Sync + 'static>(
        script: &str,
        args: impl FuncArgs,
    ) -> Result<T, Box<EvalAltResult>> {
        let mut engine = Engine::new();
        let mut scope = Scope::new();

        RhaiRegex::register(&mut engine);

        let ast = engine.compile(script).expect("Script should have compiled");
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .expect("Script should be able to run with AST");

        engine.call_fn::<T>(&mut scope, &ast, "test", args)
    }

    #[test]
    fn is_match_should_return_true_for_matching_pattern() {
        let result = run_rhai_script::<bool>(
            r#"fn test() {
                return Regex::is_match("hello world", "hello");
            }"#,
            (),
        )
        .expect("Should not error");

        assert!(result);
    }

    #[test]
    fn is_match_should_return_false_for_non_matching_pattern() {
        let result = run_rhai_script::<bool>(
            r#"fn test() {
                return Regex::is_match("hello world", "^world");
            }"#,
            (),
        )
        .expect("Should not error");

        assert!(!result);
    }

    #[test]
    fn is_match_should_return_error_for_invalid_regex() {
        let result = run_rhai_script::<bool>(
            r#"fn test() {
                return Regex::is_match("hello", "[invalid");
            }"#,
            (),
        );

        assert!(result.is_err());
    }

    #[test]
    fn is_match_should_support_regex_special_characters() {
        let result = run_rhai_script::<bool>(
            r#"fn test() {
                return Regex::is_match("test123", "\\d+");
            }"#,
            (),
        )
        .expect("Should not error");

        assert!(result);
    }

    #[test]
    fn is_match_should_return_false_for_empty_string_with_nonempty_pattern() {
        let result = run_rhai_script::<bool>(
            r#"fn test() {
                return Regex::is_match("", "\\d+");
            }"#,
            (),
        )
        .expect("Should not error");

        assert!(!result);
    }

    #[test]
    fn replace_should_replace_all_matches() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                return Regex::replace("foo bar foo", "foo", "baz");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "baz bar baz");
    }

    #[test]
    fn replace_should_support_numbered_capture_groups() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                return Regex::replace("2025-01-15", "(\\d{4})-(\\d{2})-(\\d{2})", "$2/$3/$1");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "01/15/2025");
    }

    #[test]
    fn replace_should_support_named_capture_groups() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                return Regex::replace("John Smith", "(?P<first>\\w+) (?P<last>\\w+)", "$last, $first");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "Smith, John");
    }

    #[test]
    fn replace_should_return_original_string_when_no_match() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                return Regex::replace("hello world", "\\d+", "number");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "hello world");
    }

    #[test]
    fn replace_should_return_error_for_invalid_regex() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                return Regex::replace("hello", "[invalid", "x");
            }"#,
            (),
        );

        assert!(result.is_err());
    }

    #[test]
    fn match_should_return_all_matches() {
        let result = run_rhai_script::<rhai::Array>(
            r#"fn test() {
                return Regex::matches("abc 123 def 456", "\\d+");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].clone().into_string().unwrap(), "123");
        assert_eq!(result[1].clone().into_string().unwrap(), "456");
    }

    #[test]
    fn match_should_return_empty_array_when_no_matches() {
        let result = run_rhai_script::<rhai::Array>(
            r#"fn test() {
                return Regex::matches("hello world", "\\d+");
            }"#,
            (),
        )
        .expect("Should not error");

        assert!(result.is_empty());
    }

    #[test]
    fn match_should_return_error_for_invalid_regex() {
        let result = run_rhai_script::<rhai::Array>(
            r#"fn test() {
                return Regex::matches("hello", "[invalid");
            }"#,
            (),
        );

        assert!(result.is_err());
    }

    #[test]
    fn match_should_find_complex_patterns() {
        let result = run_rhai_script::<rhai::Array>(
            r#"fn test() {
                return Regex::matches("aaa@bbb.com ccc@ddd.org", "\\w+@\\w+\\.\\w+");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].clone().into_string().unwrap(), "aaa@bbb.com");
        assert_eq!(result[1].clone().into_string().unwrap(), "ccc@ddd.org");
    }
}
