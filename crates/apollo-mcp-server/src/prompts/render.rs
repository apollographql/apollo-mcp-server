use std::collections::HashMap;

/// Perform single-pass `{{arg}}` replacement in a template string.
///
/// - Provided arguments replace `{{arg_name}}` with the supplied value.
/// - Missing optional arguments (in `defined_arg_names` but not in `arguments`) are replaced with `""`.
/// - Placeholders not matching any defined argument name are left as literal text.
/// - Argument values are treated as literals: no re-evaluation (injection prevention).
pub(crate) fn render_template(
    template: &str,
    arguments: &HashMap<String, String>,
    defined_arg_names: &[&str],
) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find("{{") {
        result.push_str(&rest[..open]);

        let after_open = &rest[open + 2..];
        if let Some(close) = after_open.find("}}") {
            let placeholder_name = &after_open[..close];

            if defined_arg_names.contains(&placeholder_name) {
                if let Some(value) = arguments.get(placeholder_name) {
                    result.push_str(value);
                }
                // Missing optional: replace with empty string (push nothing)
            } else {
                // Not a defined argument — leave as literal text
                result.push_str("{{");
                result.push_str(placeholder_name);
                result.push_str("}}");
            }

            rest = &after_open[close + 2..];
        } else {
            // No closing `}}` found — treat as literal
            result.push_str("{{");
            rest = after_open;
        }
    }

    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_substitution() {
        let args = HashMap::from([("email".to_string(), "alice@example.com".to_string())]);
        let defined = vec!["email"];
        let result = render_template("Hello {{email}}", &args, &defined);
        assert_eq!(result, "Hello alice@example.com");
    }

    #[test]
    fn multiple_arguments() {
        let args = HashMap::from([
            ("name".to_string(), "Alice".to_string()),
            ("age".to_string(), "30".to_string()),
        ]);
        let defined = vec!["name", "age"];
        let result = render_template("{{name}} is {{age}} years old", &args, &defined);
        assert_eq!(result, "Alice is 30 years old");
    }

    #[test]
    fn missing_optional_argument_replaced_with_empty() {
        let args = HashMap::new();
        let defined = vec!["email"];
        let result = render_template("User: {{email}}", &args, &defined);
        assert_eq!(result, "User: ");
    }

    #[test]
    fn injection_prevention() {
        let args = HashMap::from([("email".to_string(), "{{malicious}}".to_string())]);
        let defined = vec!["email"];
        let result = render_template("Hello {{email}}", &args, &defined);
        assert_eq!(result, "Hello {{malicious}}");
    }

    #[test]
    fn no_defined_arguments_placeholder_left_as_literal() {
        let args = HashMap::new();
        let defined: Vec<&str> = vec![];
        let result = render_template("Hello {{placeholder}}", &args, &defined);
        assert_eq!(result, "Hello {{placeholder}}");
    }

    #[test]
    fn empty_template_string() {
        let args = HashMap::new();
        let defined: Vec<&str> = vec![];
        let result = render_template("", &args, &defined);
        assert_eq!(result, "");
    }

    #[test]
    fn argument_not_in_template_no_error() {
        let args = HashMap::from([("unused".to_string(), "value".to_string())]);
        let defined = vec!["unused"];
        let result = render_template("No placeholders here", &args, &defined);
        assert_eq!(result, "No placeholders here");
    }

    #[test]
    fn multiple_occurrences_of_same_arg() {
        let args = HashMap::from([("x".to_string(), "1".to_string())]);
        let defined = vec!["x"];
        let result = render_template("{{x}} + {{x}} = 2", &args, &defined);
        assert_eq!(result, "1 + 1 = 2");
    }

    #[test]
    fn adjacent_placeholders() {
        let args = HashMap::from([
            ("a".to_string(), "X".to_string()),
            ("b".to_string(), "Y".to_string()),
        ]);
        let defined = vec!["a", "b"];
        let result = render_template("{{a}}{{b}}", &args, &defined);
        assert_eq!(result, "XY");
    }

    #[test]
    fn incomplete_placeholder_left_as_literal() {
        let args = HashMap::new();
        let defined: Vec<&str> = vec![];
        let result = render_template("Hello {{unclosed", &args, &defined);
        assert_eq!(result, "Hello {{unclosed");
    }

    #[test]
    fn multibyte_characters_around_placeholder() {
        let args = HashMap::from([("name".to_string(), "太郎".to_string())]);
        let defined = vec!["name"];
        let result = render_template("こんにちは {{name}} さん！", &args, &defined);
        assert_eq!(result, "こんにちは 太郎 さん！");
    }

    #[test]
    fn multibyte_characters_in_non_placeholder_text() {
        let args = HashMap::new();
        let defined: Vec<&str> = vec![];
        let result = render_template("日本語テキスト 🚀 絵文字", &args, &defined);
        assert_eq!(result, "日本語テキスト 🚀 絵文字");
    }

    #[test]
    fn multibyte_undefined_placeholder_preserved() {
        let args = HashMap::new();
        let defined: Vec<&str> = vec![];
        let result = render_template("前 {{未定義}} 後", &args, &defined);
        assert_eq!(result, "前 {{未定義}} 後");
    }

    #[test]
    fn empty_placeholder_name_left_as_literal() {
        let args = HashMap::new();
        let defined: Vec<&str> = vec![];
        let result = render_template("before {{}} after", &args, &defined);
        assert_eq!(result, "before {{}} after");
    }

    #[test]
    fn triple_braces_around_arg() {
        // {{ is consumed as open, so placeholder name becomes "{x", which is undefined
        let args = HashMap::from([("x".to_string(), "val".to_string())]);
        let defined = vec!["x"];
        let result = render_template("{{{x}}}", &args, &defined);
        assert_eq!(result, "{{{x}}}");
    }

    #[test]
    fn only_closing_braces_no_crash() {
        let args = HashMap::new();
        let defined: Vec<&str> = vec![];
        let result = render_template("no open }} here", &args, &defined);
        assert_eq!(result, "no open }} here");
    }

    #[test]
    fn whitespace_in_placeholder_name_not_matched() {
        let args = HashMap::from([("name".to_string(), "val".to_string())]);
        let defined = vec!["name"];
        let result = render_template("{{ name }}", &args, &defined);
        // " name " doesn't match defined arg "name", left as literal
        assert_eq!(result, "{{ name }}");
    }

    #[test]
    fn consecutive_open_braces() {
        // First {{ consumed as open, placeholder name is "{{a", undefined -> left as literal
        let args = HashMap::from([("a".to_string(), "X".to_string())]);
        let defined = vec!["a"];
        let result = render_template("{{{{a}}}}", &args, &defined);
        assert_eq!(result, "{{{{a}}}}");
    }

    #[test]
    fn template_with_only_placeholder() {
        let args = HashMap::from([("val".to_string(), "result".to_string())]);
        let defined = vec!["val"];
        let result = render_template("{{val}}", &args, &defined);
        assert_eq!(result, "result");
    }

    #[test]
    fn special_chars_in_value_preserved() {
        let args = HashMap::from([("x".to_string(), "<script>alert(1)</script>".to_string())]);
        let defined = vec!["x"];
        let result = render_template("content: {{x}}", &args, &defined);
        assert_eq!(result, "content: <script>alert(1)</script>");
    }
}
