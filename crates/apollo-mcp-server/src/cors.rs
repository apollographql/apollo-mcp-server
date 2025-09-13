//! Cross Origin Resource Sharing (CORS configuration)

use std::str::FromStr;
use std::time::Duration;

use http::request::Parts;
use http::HeaderValue;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower_http::cors;
use tower_http::cors::CorsLayer;

/// Cross origin request configuration.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct Cors {
    /// Set to true to allow any origin.
    ///
    /// Defaults to false
    /// Having this set to true is the only way to allow Origin: null.
    pub allow_any_origin: bool,

    /// Set to true to add the `Access-Control-Allow-Credentials` header.
    pub allow_credentials: bool,

    /// The headers to allow.
    ///
    /// If this value is not set, the router will mirror client's `Access-Control-Request-Headers`.
    ///
    /// Note that if you set headers here,
    /// you also want to have a look at your `CSRF` plugins configuration,
    /// and make sure you either:
    /// - accept `x-apollo-operation-name` AND / OR `apollo-require-preflight`
    /// - defined `csrf` required headers in your yml configuration, as shown in the
    ///   `examples/cors-and-csrf/custom-headers.router.yaml` files.
    pub allow_headers: Vec<String>,

    /// Which response headers should be made available to scripts running in the browser,
    /// in response to a cross-origin request.
    pub expose_headers: Option<Vec<String>>,

    /// The origin(s) to allow requests from.
    /// Defaults to `https://studio.apollographql.com/` for Apollo Studio.
    pub origins: Vec<String>,

    /// `Regex`es you want to match the origins against to determine if they're allowed.
    /// Defaults to an empty list.
    /// Note that `origins` will be evaluated before `match_origins`
    pub match_origins: Option<Vec<String>>,

    /// Allowed request methods. Defaults to GET, POST, OPTIONS.
    pub methods: Vec<String>,

    /// The `Access-Control-Max-Age` header value in time units
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    pub max_age: Option<Duration>,
}

impl Default for Cors {
    fn default() -> Self {
        Self::builder().build()
    }
}

fn default_origins() -> Vec<String> {
    vec!["https://studio.apollographql.com".into()]
}

fn default_cors_methods() -> Vec<String> {
    vec!["GET".into(), "POST".into(), "OPTIONS".into()]
}

#[buildstructor::buildstructor]
impl Cors {
    #[builder]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        allow_any_origin: Option<bool>,
        allow_credentials: Option<bool>,
        allow_headers: Option<Vec<String>>,
        expose_headers: Option<Vec<String>>,
        origins: Option<Vec<String>>,
        match_origins: Option<Vec<String>>,
        methods: Option<Vec<String>>,
        max_age: Option<Duration>,
    ) -> Self {
        Self {
            expose_headers,
            match_origins,
            max_age,
            origins: origins.unwrap_or_else(default_origins),
            methods: methods.unwrap_or_else(default_cors_methods),
            allow_any_origin: allow_any_origin.unwrap_or_default(),
            allow_credentials: allow_credentials.unwrap_or_default(),
            allow_headers: allow_headers.unwrap_or_default(),
        }
    }
}

impl Cors {
    pub fn into_layer(self) -> Result<CorsLayer, String> {
        // Ensure configuration is valid before creating CorsLayer
        self.ensure_usable_cors_rules()?;

        let allow_headers = if self.allow_headers.is_empty() {
            cors::AllowHeaders::mirror_request()
        } else {
            cors::AllowHeaders::list(self.allow_headers.iter().filter_map(|header| {
                header
                    .parse()
                    .map_err(|_| tracing::error!("header name '{header}' is not valid"))
                    .ok()
            }))
        };
        let cors = CorsLayer::new()
            .vary([])
            .allow_credentials(self.allow_credentials)
            .allow_headers(allow_headers)
            .expose_headers(cors::ExposeHeaders::list(
                self.expose_headers
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|header| {
                        header
                            .parse()
                            .map_err(|_| tracing::error!("header name '{header}' is not valid"))
                            .ok()
                    }),
            ))
            .allow_methods(cors::AllowMethods::list(self.methods.iter().filter_map(
                |method| {
                    method
                        .parse()
                        .map_err(|_| tracing::error!("method '{method}' is not valid"))
                        .ok()
                },
            )));
        let cors = if let Some(max_age) = self.max_age {
            cors.max_age(max_age)
        } else {
            cors
        };

        if self.allow_any_origin {
            Ok(cors.allow_origin(cors::Any))
        } else if let Some(match_origins) = self.match_origins {
            let regexes = match_origins
                .into_iter()
                .filter_map(|regex| {
                    Regex::from_str(regex.as_str())
                        .map_err(|_| tracing::error!("origin regex '{regex}' is not valid"))
                        .ok()
                })
                .collect::<Vec<_>>();

            Ok(cors.allow_origin(cors::AllowOrigin::predicate(
                move |origin: &HeaderValue, _: &Parts| {
                    origin
                        .to_str()
                        .map(|o| {
                            self.origins.iter().any(|origin| origin.as_str() == o)
                                || regexes.iter().any(|regex| regex.is_match(o))
                        })
                        .unwrap_or_default()
                },
            )))
        } else {
            Ok(cors.allow_origin(cors::AllowOrigin::list(
                self.origins.into_iter().filter_map(|origin| {
                    origin
                        .parse()
                        .map_err(|_| tracing::error!("origin '{origin}' is not valid"))
                        .ok()
                }),
            )))
        }
    }

    // This is cribbed from the similarly named function in tower-http. The version there
    // asserts that CORS rules are useable, which results in a panic if they aren't. We
    // don't want the router to panic in such cases, so this function returns an error
    // with a message describing what the problem is.
    fn ensure_usable_cors_rules(&self) -> Result<(), &'static str> {
        if self.origins.iter().any(|x| x == "*") {
            return Err("Invalid CORS configuration: use `allow_any_origin: true` to set `Access-Control-Allow-Origin: *`");
        }
        if self.allow_credentials {
            if self.allow_headers.iter().any(|x| x == "*") {
                return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                        with `Access-Control-Allow-Headers: *`");
            }

            if self.methods.iter().any(|x| x == "*") {
                return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                    with `Access-Control-Allow-Methods: *`");
            }

            if self.allow_any_origin {
                return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                    with `allow_any_origin: true`");
            }

            if let Some(headers) = &self.expose_headers {
                if headers.iter().any(|x| x == "*") {
                    return Err("Invalid CORS configuration: Cannot combine `Access-Control-Allow-Credentials: true` \
                        with `Access-Control-Expose-Headers: *`");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Cors::default();
        assert!(!config.allow_any_origin);
        assert!(!config.allow_credentials);
        assert_eq!(config.origins, vec!["https://studio.apollographql.com"]);
        assert_eq!(config.methods, vec!["GET", "POST", "OPTIONS"]);
        assert!(config.allow_headers.is_empty());
        assert!(config.expose_headers.is_none());
        assert!(config.max_age.is_none());
        assert!(config.match_origins.is_none());
    }

    #[test]
    fn test_valid_configuration() {
        let config = Cors {
            allow_any_origin: false,
            allow_credentials: false,
            allow_headers: vec!["content-type".into(), "authorization".into()],
            expose_headers: Some(vec!["x-custom-header".into()]),
            methods: vec!["GET".into(), "POST".into()],
            max_age: Some(Duration::from_secs(3600)),
            origins: vec!["https://example.com".into()],
            match_origins: None,
        };

        let result = config.into_layer();
        assert!(result.is_ok());
    }

    #[test]
    fn test_wildcard_origin_rejected() {
        let config = Cors {
            origins: vec!["*".into()],
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("use `allow_any_origin: true`"));
    }

    #[test]
    fn test_trailing_slash_origin_accepted() {
        // Note: Router version doesn't validate trailing slashes
        let config = Cors {
            origins: vec!["https://example.com/".into()],
            ..Default::default()
        };

        let result = config.into_layer();
        // Should be OK now since we removed trailing slash validation
        assert!(result.is_ok());
    }

    #[test]
    fn test_credentials_with_wildcard_origin_rejected() {
        let config = Cors {
            allow_any_origin: true,
            allow_credentials: true,
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("Cannot combine `Access-Control-Allow-Credentials: true`")
        );
    }

    #[test]
    fn test_credentials_with_wildcard_headers_rejected() {
        let config = Cors {
            allow_credentials: true,
            allow_headers: vec!["*".into()],
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("Cannot combine `Access-Control-Allow-Credentials: true`")
        );
    }

    #[test]
    fn test_credentials_with_wildcard_methods_rejected() {
        let config = Cors {
            allow_credentials: true,
            methods: vec!["*".into()],
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("Cannot combine `Access-Control-Allow-Credentials: true`")
        );
    }

    #[test]
    fn test_invalid_method_filtered() {
        // Router version filters out invalid methods instead of failing
        let config = Cors {
            methods: vec!["INVALID\nMETHOD".into(), "GET".into()],
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_ok()); // Should succeed, invalid method is filtered out
    }

    #[test]
    fn test_invalid_header_filtered() {
        // Router version filters out invalid headers instead of failing
        let config = Cors {
            allow_headers: vec!["invalid\nheader".into(), "content-type".into()],
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_ok()); // Should succeed, invalid header is filtered out
    }

    #[test]
    fn test_allow_any_origin() {
        let config = Cors {
            allow_any_origin: true,
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_ok());
    }

    #[test]
    fn test_max_age_configuration() {
        let config = Cors {
            max_age: Some(Duration::from_secs(7200)),
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_origins_with_defaults() {
        let config = Cors {
            origins: vec![],
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_ok());
    }

    #[test]
    fn test_regex_pattern_matching() {
        let config = Cors {
            origins: vec!["https://example.com".into()],
            match_origins: Some(vec![r"https://.*\.example\.com".into()]),
            ..Default::default()
        };

        let result = config.into_layer();
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_regex_pattern() {
        let config = Cors {
            origins: vec![],
            match_origins: Some(vec!["[invalid regex".into()]),
            ..Default::default()
        };

        // Should still succeed but log error (Router behavior)
        let result = config.into_layer();
        assert!(result.is_ok());
    }
}