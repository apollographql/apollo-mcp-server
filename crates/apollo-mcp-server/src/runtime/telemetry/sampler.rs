use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, untagged)]
pub(crate) enum SamplerOption {
    /// Sample a given fraction. Fractions >= 1 will always sample.
    RatioBased(f64),
    Always(Sampler),
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Sampler {
    /// Always sample
    AlwaysOn,
    /// Never sample
    AlwaysOff,
}

impl From<Sampler> for opentelemetry_sdk::trace::Sampler {
    fn from(s: Sampler) -> Self {
        match s {
            Sampler::AlwaysOn => opentelemetry_sdk::trace::Sampler::AlwaysOn,
            Sampler::AlwaysOff => opentelemetry_sdk::trace::Sampler::AlwaysOff,
        }
    }
}

impl From<SamplerOption> for opentelemetry_sdk::trace::Sampler {
    fn from(s: SamplerOption) -> Self {
        match s {
            SamplerOption::Always(s) => s.into(),
            SamplerOption::RatioBased(ratio) => {
                opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(ratio)
            }
        }
    }
}

impl Default for SamplerOption {
    fn default() -> Self {
        SamplerOption::Always(Sampler::AlwaysOn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_always_on_maps_to_otel_always_on() {
        assert!(matches!(
            Sampler::AlwaysOn.into(),
            opentelemetry_sdk::trace::Sampler::AlwaysOn
        ));
    }

    #[test]
    fn sampler_always_off_maps_to_otel_always_off() {
        assert!(matches!(
            Sampler::AlwaysOff.into(),
            opentelemetry_sdk::trace::Sampler::AlwaysOff
        ));
    }

    #[test]
    fn sampler_option_always_on_maps_to_otel_always_on() {
        assert!(matches!(
            SamplerOption::Always(Sampler::AlwaysOn).into(),
            opentelemetry_sdk::trace::Sampler::AlwaysOn
        ));
    }

    #[test]
    fn sampler_option_always_off_maps_to_otel_always_off() {
        assert!(matches!(
            SamplerOption::Always(Sampler::AlwaysOff).into(),
            opentelemetry_sdk::trace::Sampler::AlwaysOff
        ));
    }

    #[test]
    fn sampler_option_ratio_based_maps_to_otel_ratio_based_sampler() {
        assert!(matches!(
            SamplerOption::RatioBased(0.5).into(),
            opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(0.5)
        ));
    }

    #[test]
    fn default_sampler_option_is_always_on() {
        assert!(matches!(
            SamplerOption::default(),
            SamplerOption::Always(Sampler::AlwaysOn)
        ));
    }
}
