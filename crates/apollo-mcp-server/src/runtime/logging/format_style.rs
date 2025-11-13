use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, JsonSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Log output format style. Maps to a format from tracing-subscriber.
/// See the [tracing_subscriber::fmt documentation](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/index.html) for more info.
pub enum FormatStyle {
    #[default]
    /// The default format. Uses the full format from tracing-subscriber that emits
    /// human-readable, single-line logs.
    Full,
    /// Uses the compact format from tracing-subscriber optimized for short line lengths.
    Compact,
    /// Uses the json format from tracing-subscriber that outputs newline-delimited json logs.
    Json,
    /// Uses the pretty format from tracing-subscriber that emits excessively pretty, multi-line
    /// logs optimized for human readability.
    Pretty,
}

#[cfg(test)]
mod tests {
    use crate::runtime::logging::format_style::FormatStyle;
    use rstest::rstest;
    use serde::Deserialize;
    use serde::de::value::{Error, StrDeserializer};

    #[test]
    fn full_style_returned_as_default() {
        assert_eq!(FormatStyle::default(), FormatStyle::Full);
    }

    #[rstest]
    #[case("full", FormatStyle::Full)]
    #[case("compact", FormatStyle::Compact)]
    #[case("json", FormatStyle::Json)]
    #[case("pretty", FormatStyle::Pretty)]
    fn direct_deserialization_deserializes_into_the_correct_format_style(
        #[case] value: &str,
        #[case] expected: FormatStyle,
    ) {
        let de = StrDeserializer::<Error>::new(value);
        let actual: FormatStyle = FormatStyle::deserialize(de).unwrap();
        assert_eq!(actual, expected);
    }

    #[rstest]
    #[case("full", FormatStyle::Full)]
    #[case("compact", FormatStyle::Compact)]
    #[case("json", FormatStyle::Json)]
    #[case("pretty", FormatStyle::Pretty)]
    fn yaml_deserialization_deserializes_into_the_correct_format_style(
        #[case] yaml_value: &str,
        #[case] expected: FormatStyle,
    ) {
        let actual: FormatStyle = serde_yaml::from_str(yaml_value).unwrap();
        assert_eq!(actual, expected);
    }

    #[rstest]
    #[case("")]
    #[case(" ")]
    fn yaml_deserialization_of_empty_value_results_in_eof_error(#[case] empty_str: &str) {
        let result: Result<FormatStyle, _> = serde_yaml::from_str(empty_str);
        assert!(result.is_err());
        assert!(
            result
                .expect_err("expected an error for invalid format style")
                .to_string()
                .contains("EOF while parsing a value")
        );
    }

    #[rstest]
    #[case("Full")]
    #[case("Compact")]
    #[case("JSON")]
    #[case("Pretty")]
    #[case("invalid")]
    fn yaml_deserialization_of_invalid_style_results_in_an_unknown_variant_error(
        #[case] invalid_value: &str,
    ) {
        let result: Result<FormatStyle, _> = serde_yaml::from_str(invalid_value);
        assert!(result.is_err());
        assert!(
            result
                .expect_err("expected an error for invalid format style")
                .to_string()
                .contains("unknown variant")
        );
    }

    #[rstest]
    #[case("Full")]
    #[case("Compact")]
    #[case("JSON")]
    #[case("Pretty")]
    #[case("invalid")]
    fn direct_deserialization_of_invalid_style_results_in_an_unknown_variant_error(
        #[case] invalid_value: &str,
    ) {
        let de = StrDeserializer::<Error>::new(invalid_value);
        let err = FormatStyle::deserialize(de).unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }
}
