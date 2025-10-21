use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Default, JsonSchema, Clone, PartialEq, Eq)]
pub enum FormatStyle {
    #[default]
    Full,
    Compact,
    Json,
    Pretty,
}

impl<'de> Deserialize<'de> for FormatStyle {
    /// Case-insensitive deserializer for str to FormatStyle
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "full" => Ok(Self::Full),
            "compact" => Ok(Self::Compact),
            "json" => Ok(Self::Json),
            "pretty" => Ok(Self::Pretty),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["full", "compact", "json", "pretty"],
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::logging::format_style::FormatStyle;
    use crate::runtime::telemetry::MetricTemporality;
    use rstest::rstest;
    use serde::Deserialize;
    use serde::de::value::{Error, StrDeserializer};

    #[test]
    fn full_style_returned_as_default() {
        assert_eq!(FormatStyle::default(), FormatStyle::Full);
    }

    #[rstest]
    #[case::lower_cumulative("full", FormatStyle::Full)]
    #[case::title_cumulative("Full", FormatStyle::Full)]
    #[case::upper_cumulative("FULL", FormatStyle::Full)]
    #[case::lower_delta("compact", FormatStyle::Compact)]
    #[case::title_delta("Compact", FormatStyle::Compact)]
    #[case::upper_delta("COMPACT", FormatStyle::Compact)]
    #[case::lower_lowmemory("json", FormatStyle::Json)]
    #[case::title_lowmemory("Json", FormatStyle::Json)]
    #[case::upper_lowmemory("JSON", FormatStyle::Json)]
    #[case::lower_lowmemory("pretty", FormatStyle::Pretty)]
    #[case::title_lowmemory("Pretty", FormatStyle::Pretty)]
    #[case::upper_lowmemory("PRETTY", FormatStyle::Pretty)]
    fn direct_deserialization_deserializes_into_the_correct_format_style(
        #[case] value: &str,
        #[case] expected: FormatStyle,
    ) {
        let de = StrDeserializer::<Error>::new(value);
        let actual: FormatStyle = FormatStyle::deserialize(de).unwrap();
        assert_eq!(actual, expected);
    }

    #[rstest]
    #[case::lower_cumulative("full", FormatStyle::Full)]
    #[case::title_cumulative("Full", FormatStyle::Full)]
    #[case::upper_cumulative("FULL", FormatStyle::Full)]
    #[case::lower_delta("compact", FormatStyle::Compact)]
    #[case::title_delta("Compact", FormatStyle::Compact)]
    #[case::upper_delta("COMPACT", FormatStyle::Compact)]
    #[case::lower_lowmemory("json", FormatStyle::Json)]
    #[case::title_lowmemory("Json", FormatStyle::Json)]
    #[case::upper_lowmemory("JSON", FormatStyle::Json)]
    #[case::lower_lowmemory("pretty", FormatStyle::Pretty)]
    #[case::title_lowmemory("Pretty", FormatStyle::Pretty)]
    #[case::upper_lowmemory("PRETTY", FormatStyle::Pretty)]
    fn yaml_deserialization_deserializes_into_the_correct_format_style(
        #[case] yaml_value: &str,
        #[case] expected: FormatStyle,
    ) {
        let actual: FormatStyle = serde_yaml::from_str(yaml_value).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn yaml_deserialization_of_invalid_style_results_in_an_unknown_variant_error() {
        let result: Result<MetricTemporality, _> = serde_yaml::from_str("invalid");
        assert!(result.is_err());
        assert!(
            result
                .expect_err("expected an error for invalid format style")
                .to_string()
                .contains("unknown variant")
        );
    }

    #[test]
    fn direct_deserialization_of_invalid_style_results_in_an_unknown_variant_error() {
        let de = StrDeserializer::<Error>::new("invalid");
        let err = MetricTemporality::deserialize(de).unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }
}
