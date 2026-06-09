use std::ops::Deref;

use headers::{Authorization, authorization::Bearer};
use jsonwebtoken::{Algorithm, Validation, decode, decode_header, jwk};
use jwks::Jwk;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use url::Url;

/// A validated authentication token
///
/// Note: This is used as a marker to ensure that we have validated this
/// separately from just reading the header itself.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ValidToken {
    pub(crate) token: Authorization<Bearer>,
    pub(crate) scopes: Vec<String>,
}

impl Deref for ValidToken {
    type Target = Authorization<Bearer>;

    fn deref(&self) -> &Self::Target {
        &self.token
    }
}

/// A signing key resolved from an authorization server, together with that
/// server's discovered issuer identifier.
///
/// `issuer` is the `iss` value the authorization server advertises in its
/// discovery metadata. It is used to bind issuer validation to the server
/// whose JWKS verified the signature: a token's `iss` claim must equal this
/// value, so a token signed by one configured server cannot pass by claiming
/// a different configured server's issuer.
pub(super) struct VerificationKey {
    pub(super) jwk: Jwk,
    pub(super) issuer: String,
}

/// Resolves a signing key for a `(server, key_id)` pair, returning it together
/// with the issuer that server advertises in discovery.
///
/// This is the single seam that production fetches over the network and tests
/// substitute in memory. Everything else about validation lives in
/// [`TokenValidator`], so adding a new validation input does not touch this
/// trait or its implementations.
pub(super) trait KeyResolver {
    /// Fetch the signing key by its ID, along with the issuing server's
    /// discovered issuer identifier. Returns `None` when the server does not
    /// serve a key with that `key_id`.
    async fn resolve_key(&self, server: &Url, key_id: &str) -> Option<VerificationKey>;
}

/// Validates bearer JWTs against the configured audiences, issuers, and upstream
/// servers, resolving signing keys through `keys`.
pub(super) struct TokenValidator<'a, R: KeyResolver> {
    /// Accepted audiences. Ignored when `allow_any_audience` is set.
    pub(super) audiences: &'a [String],
    /// Accepted issuers (empty = skip issuer validation).
    pub(super) issuers: &'a [String],
    /// Skip audience validation entirely.
    pub(super) allow_any_audience: bool,
    /// Upstream authorization servers to try, in order.
    pub(super) servers: &'a [Url],
    /// Resolves signing keys (the network seam).
    pub(super) keys: R,
}

impl<R: KeyResolver> TokenValidator<'_, R> {
    /// Attempt to validate a token against the configured rules.
    pub(super) async fn validate(&self, token: Authorization<Bearer>) -> Option<ValidToken> {
        /// Claims which must be present in the JWT (and must match validation)
        /// in order for a JWT to be considered valid.
        ///
        /// See: https://auth0.com/docs/secure/tokens/json-web-tokens/json-web-token-claims#registered-claims
        #[derive(Clone, Debug, Serialize, Deserialize)]
        pub struct Claims {
            /// The intended audience of this token.
            /// Can be either a single string or an array of strings per JWT spec. (https://datatracker.ietf.org/doc/html/rfc7519#section-4.1.3)
            /// Some providers (e.g., AWS Cognito) omit `aud` entirely in access tokens,
            /// so this field defaults to an empty vec when absent.
            #[serde(default, deserialize_with = "deserialize_audience")]
            pub aud: Vec<String>,

            /// The issuer of this token (`iss`). Optional in the struct so tokens
            /// without it still deserialize; enforced in `validate` when issuers are configured.
            #[serde(default)]
            pub iss: Option<String>,

            /// The user who owns this token
            pub sub: String,

            /// OAuth scope claim (space-separated list per RFC 6749)
            #[serde(default)]
            pub scope: Option<String>,

            /// Non-standard scope claim. Okta emits this as an array of
            /// strings; Microsoft Entra emits it as a space-separated string.
            /// Used as a fallback when the RFC 9068 `scope` claim is absent.
            #[serde(default)]
            pub scp: Option<ScpClaim>,
        }

        #[derive(Clone, Debug, Serialize, Deserialize)]
        #[serde(untagged)]
        enum ScpClaim {
            Array(Vec<String>),
            String(String),
        }

        impl Claims {
            /// Resolve the scopes granted by this token, preferring the
            /// RFC 9068 `scope` claim and falling back to the `scp` claim
            /// used by Okta (array) and Microsoft Entra (space-separated string).
            fn scopes(self) -> Vec<String> {
                match (self.scope, self.scp) {
                    (Some(s), _) | (None, Some(ScpClaim::String(s))) => {
                        s.split_whitespace().map(String::from).collect()
                    }
                    (None, Some(ScpClaim::Array(v))) => v,
                    (None, None) => Vec::new(),
                }
            }
        }

        fn deserialize_audience<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            #[serde(untagged)]
            enum Audience {
                Single(String),
                Multiple(Vec<String>),
            }

            Ok(match Option::<Audience>::deserialize(deserializer)? {
                Some(Audience::Single(s)) => vec![s],
                Some(Audience::Multiple(v)) => v,
                None => Vec::new(),
            })
        }

        let jwt = token.token();
        let header = decode_header(jwt).ok()?;
        let key_id = header.kid.as_ref()?;

        for server in self.servers {
            let Some(VerificationKey {
                jwk,
                issuer: discovered_issuer,
            }) = self.keys.resolve_key(server, key_id).await
            else {
                continue;
            };

            let validation = {
                let Some(alg) = jwk.alg else {
                    warn!("Skipping JWK with no algorithm specified");
                    continue;
                };
                let mut val = Validation::new(match alg {
                    jwk::KeyAlgorithm::HS256 => Algorithm::HS256,
                    jwk::KeyAlgorithm::HS384 => Algorithm::HS384,
                    jwk::KeyAlgorithm::HS512 => Algorithm::HS512,
                    jwk::KeyAlgorithm::ES256 => Algorithm::ES256,
                    jwk::KeyAlgorithm::ES384 => Algorithm::ES384,
                    jwk::KeyAlgorithm::RS256 => Algorithm::RS256,
                    jwk::KeyAlgorithm::RS384 => Algorithm::RS384,
                    jwk::KeyAlgorithm::RS512 => Algorithm::RS512,
                    jwk::KeyAlgorithm::PS256 => Algorithm::PS256,
                    jwk::KeyAlgorithm::PS384 => Algorithm::PS384,
                    jwk::KeyAlgorithm::PS512 => Algorithm::PS512,
                    jwk::KeyAlgorithm::EdDSA => Algorithm::EdDSA,

                    // No other validation key type is supported by this library, so we
                    // warn and fail if we encounter one.
                    other => {
                        warn!("Skipping JWT signed by unsupported algorithm: {other:?}");
                        continue;
                    }
                });
                if self.allow_any_audience {
                    val.validate_aud = false;
                } else {
                    val.set_audience(self.audiences);
                }

                if !self.issuers.is_empty() {
                    val.set_issuer(self.issuers);
                }

                val
            };

            match decode::<Claims>(jwt, &jwk.decoding_key, &validation) {
                Ok(token_data) => {
                    // When audience validation is enabled, explicitly reject tokens
                    // with a missing `aud` claim. The `jsonwebtoken` crate skips its
                    // own audience check when the claim is absent from the raw JWT,
                    // so we enforce it here.
                    if !self.allow_any_audience && token_data.claims.aud.is_empty() {
                        warn!("Token is missing the required `aud` claim");
                        break;
                    }
                    if !self.issuers.is_empty() {
                        // When issuer validation is enabled, explicitly reject tokens
                        // with a missing `iss` claim. The `jsonwebtoken` crate skips its
                        // own issuer check when the claim is absent from the raw JWT,
                        // so we enforce it here.
                        let Some(token_issuer) = token_data.claims.iss.as_deref() else {
                            warn!("Token is missing the required `iss` claim");
                            break;
                        };

                        // Bind the issuer to the server whose JWKS verified the
                        // signature: the token's `iss` must equal that server's
                        // discovered issuer identifier. This prevents a token signed
                        // by one configured server from being accepted while claiming
                        // a different configured server's issuer.
                        if discovered_issuer != token_issuer {
                            warn!(
                                token_issuer = %token_issuer,
                                server_issuer = %discovered_issuer,
                                "Token `iss` does not match the issuer of the server that signed it"
                            );
                            break;
                        }
                    }
                    return Some(ValidToken {
                        token,
                        scopes: token_data.claims.scopes(),
                    });
                }
                Err(e) => warn!("Token failed validation with error: {e}"),
            };
        }

        info!("Token did not pass validation");
        None
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use headers::{Authorization, authorization::Bearer};
    use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode, jwk::KeyAlgorithm};
    use jwks::Jwk;
    use serde::Serialize;
    use tracing_test::traced_test;
    use url::Url;

    use super::{KeyResolver, TokenValidator, ValidToken, VerificationKey};

    /// A single upstream server in the stub resolver: its URL, the one
    /// `(kid, jwk)` it serves, and the issuer it advertises in discovery.
    struct TestServer {
        url: Url,
        key_pair: (String, Jwk),
        discovered_issuer: String,
    }

    /// In-memory [`KeyResolver`] that stands in for the network in tests.
    struct StubKeyResolver<'a> {
        servers: &'a [TestServer],
    }

    impl KeyResolver for StubKeyResolver<'_> {
        async fn resolve_key(&self, server: &Url, key_id: &str) -> Option<VerificationKey> {
            // Find the requested server, then return its key only if the `kid` matches.
            let test_server = self.servers.iter().find(|s| &s.url == server)?;
            test_server.key_pair.0.eq(key_id).then(|| VerificationKey {
                jwk: test_server.key_pair.1.clone(),
                issuer: test_server.discovered_issuer.clone(),
            })
        }
    }

    /// Thin test harness that owns the validation inputs and delegates to the
    /// real [`TokenValidator`] through a [`StubKeyResolver`]. Keeps the test
    /// call sites concise while exercising the production validation path.
    struct TestTokenValidator {
        audiences: Vec<String>,
        issuers: Vec<String>,
        allow_any_audience: bool,
        servers: Vec<TestServer>,
        /// Parsed server URLs, kept index-aligned with `servers`.
        server_urls: Vec<Url>,
    }

    impl TestTokenValidator {
        fn new(
            audiences: Vec<String>,
            issuers: Vec<String>,
            allow_any_audience: bool,
            servers: Vec<TestServer>,
        ) -> Self {
            let server_urls = servers.iter().map(|s| s.url.clone()).collect();
            Self {
                audiences,
                issuers,
                allow_any_audience,
                servers,
                server_urls,
            }
        }

        /// Convenience for the common single-server case. The discovered issuer
        /// defaults to the server URL with any trailing slash trimmed, matching
        /// how a compliant server advertises its issuer identifier.
        fn single(
            audiences: Vec<String>,
            issuers: Vec<String>,
            allow_any_audience: bool,
            key_pair: (String, Jwk),
            server: Url,
        ) -> Self {
            let discovered_issuer = server.as_str().trim_end_matches('/').to_string();
            Self::new(
                audiences,
                issuers,
                allow_any_audience,
                vec![TestServer {
                    url: server,
                    key_pair,
                    discovered_issuer,
                }],
            )
        }

        /// Run the real [`TokenValidator`] against this harness's inputs.
        async fn validate(&self, token: Authorization<Bearer>) -> Option<ValidToken> {
            TokenValidator {
                audiences: &self.audiences,
                issuers: &self.issuers,
                allow_any_audience: self.allow_any_audience,
                servers: &self.server_urls,
                keys: StubKeyResolver {
                    servers: &self.servers,
                },
            }
            .validate(token)
            .await
        }
    }

    /// Creates a key for signing / verifying JWTs
    fn create_key(base64_secret: &str) -> (EncodingKey, DecodingKey) {
        let encode =
            EncodingKey::from_base64_secret(base64_secret).expect("create valid encoding key");
        let decode =
            DecodingKey::from_base64_secret(base64_secret).expect("create valid decoding key");

        (encode, decode)
    }

    fn create_jwt(
        key_id: String,
        key: EncodingKey,
        audience: String,
        expires_at: i64,
    ) -> Authorization<Bearer> {
        #[derive(Serialize)]
        struct Claims {
            aud: String,
            exp: i64,
            sub: String,
        }

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id);

            h
        };
        let token = encode(
            &header,
            &Claims {
                aud: audience,
                exp: expires_at,
                sub: "test user".to_string(),
            },
            &key,
        )
        .expect("encode JWT");

        Authorization::bearer(&token).expect("create bearer token")
    }

    #[tokio::test]
    async fn it_validates_jwt() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, audience.clone(), in_the_future);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![], false, (key_id, jwk), server);

        let token = jwt.token().to_string();
        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .token
                .token(),
            token
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_different_key() {
        let key_id = "some-example-id".to_string();
        let (_, decode_key) = create_key("CAFED00D");
        let (bad_encode_key, _) = create_key("DEADC0DE");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt(
            key_id.clone(),
            bad_encode_key,
            audience.clone(),
            in_the_future,
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![], false, (key_id, jwk), server);

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("InvalidSignature"))
                .then_some(())
                .ok_or("Expected warning for validation failure".to_string())
        });
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_expired() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("F0CACC1A");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_past = chrono::Utc::now().timestamp() - 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, audience.clone(), in_the_past);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![], false, (key_id, jwk), server);

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("ExpiredSignature"))
                .then_some(())
                .ok_or("Expected warning for validation failure".to_string())
        });
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_different_audience() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("F0CACC1A");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let bad_audience = "not-test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, bad_audience, in_the_future);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![], false, (key_id, jwk), server);

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("InvalidAudience"))
                .then_some(())
                .ok_or("Expected warning for validation failure".to_string())
        });
    }

    #[tokio::test]
    async fn it_validates_jwt_with_array_audience() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "aud": ["test-audience", "another-audience"],
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![], false, (key_id, jwk), server);

        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .token
                .token(),
            token
        );
    }

    #[tokio::test]
    async fn it_validates_jwt_with_allow_any_audience() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "any-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, audience, in_the_future);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        // allow_any_audience should skip audience validation entirely
        let test_validator =
            TestTokenValidator::single(vec![], vec![], true, (key_id, jwk), server);

        let token = jwt.token().to_string();
        assert_eq!(test_validator.validate(jwt).await.unwrap().0.token(), token);
    }

    #[tokio::test]
    async fn it_validates_jwt_with_missing_audience_when_allow_any() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        // Create a JWT without the `aud` claim (like AWS Cognito access tokens)
        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![], vec![], true, (key_id, jwk), server);

        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .token
                .token(),
            token
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_missing_audience_when_audience_required() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        // Create a JWT without the `aud` claim
        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        // With allow_any_audience=false and configured audiences, missing aud should fail
        let test_validator = TestTokenValidator::single(
            vec!["expected-audience".to_string()],
            vec![],
            false,
            (key_id, jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("missing the required `aud` claim"))
                .then_some(())
                .ok_or("Expected warning for missing aud claim".to_string())
        });
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_array_audience_with_no_matches() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let expected_audience = "expected-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "aud": ["wrong-audience-1", "wrong-audience-2"],
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec![expected_audience],
            vec![],
            false,
            (key_id, jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("InvalidAudience"))
                .then_some(())
                .ok_or("Expected warning for validation failure".to_string())
        });
    }

    #[tokio::test]
    async fn it_validates_jwt_with_matching_issuer() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let issuer = "https://auth.example.com".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "aud": "test-audience",
            "iss": "https://auth.example.com",
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![issuer], false, (key_id, jwk), server);

        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .token
                .token(),
            token
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_different_issuer() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let issuer = "https://auth.example.com".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "aud": "test-audience",
            "iss": "https://evil.example.com",
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![issuer], false, (key_id, jwk), server);

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("InvalidIssuer"))
                .then_some(())
                .ok_or("Expected warning for validation failure".to_string())
        });
    }

    #[tokio::test]
    async fn it_validates_jwt_when_issuer_matches_any_configured() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        // The configured allowlist holds two issuers. The signing server
        // advertises the SECOND one as its discovered issuer, and the token's
        // `iss` matches it — exercising both the allowlist (any-match) and the
        // discovery binding to the actual signer.
        let claims = json!({
            "aud": "test-audience",
            "iss": "https://auth.other.com",
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::new(
            vec![audience],
            vec![
                "https://auth.example.com".to_string(),
                "https://auth.other.com".to_string(),
            ],
            false,
            vec![TestServer {
                url: server,
                key_pair: (key_id, jwk),
                discovered_issuer: "https://auth.other.com".to_string(),
            }],
        );

        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .token
                .token(),
            token
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_missing_issuer_when_issuer_required() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let issuer = "https://auth.example.com".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        // Create a JWT without the `iss` claim
        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "aud": "test-audience",
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        // With configured issuers, a missing iss claim should fail
        let test_validator =
            TestTokenValidator::single(vec![audience], vec![issuer], false, (key_id, jwk), server);

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("missing the required `iss` claim"))
                .then_some(())
                .ok_or("Expected warning for missing iss claim".to_string())
        });
    }

    #[tokio::test]
    async fn it_validates_jwt_with_no_issuers_configured() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        // No `iss` claim at all - should still be valid when issuers are not configured
        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };

        let claims = json!({
            "aud": "test-audience",
            "exp": in_the_future,
            "sub": "test user"
        });

        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        // Empty issuers list - issuer validation is skipped (backward compatible)
        let test_validator =
            TestTokenValidator::single(vec![audience], vec![], false, (key_id, jwk), server);

        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .token
                .token(),
            token
        );
    }

    // --- Multi-server issuer binding ---------------------------------------
    //
    // These exercise the cross-server case: issuer validation is bound to the
    // discovered issuer of the server whose JWKS verified the signature, so a
    // token signed by one server cannot pass by claiming another server's
    // issuer — even when both issuers are in the configured allowlist.

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_token_signed_by_one_server_claiming_another_servers_issuer() {
        use serde_json::json;

        // Two servers share the same `kid` but use different signing secrets —
        // the realistic collision that makes a cross-server claim worth testing.
        let shared_kid = "shared-kid".to_string();
        let (server_a_encode, server_a_decode) = create_key("DEADBEEF");
        let (_server_b_encode, server_b_decode) = create_key("CAFED00D");

        let server_a_url = Url::from_str("https://auth-a.example.com").expect("valid server A URL");
        let server_b_url = Url::from_str("https://auth-b.example.com").expect("valid server B URL");

        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        // Token is signed by SERVER A's key, but claims SERVER B's issuer.
        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(shared_kid.clone());
            h
        };
        let claims = json!({
            "aud": "test-audience",
            "iss": "https://auth-b.example.com",
            "exp": in_the_future,
            "sub": "test user"
        });
        let token = encode(&header, &claims, &server_a_encode).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        // Both issuers are in the allowlist, so the cross-server protection must
        // come from the discovery binding, not the allowlist.
        let test_validator = TestTokenValidator::new(
            vec!["test-audience".to_string()],
            vec![
                "https://auth-a.example.com".to_string(),
                "https://auth-b.example.com".to_string(),
            ],
            false,
            vec![
                TestServer {
                    url: server_a_url,
                    key_pair: (
                        shared_kid.clone(),
                        Jwk {
                            alg: Some(KeyAlgorithm::HS512),
                            decoding_key: server_a_decode,
                        },
                    ),
                    discovered_issuer: "https://auth-a.example.com".to_string(),
                },
                TestServer {
                    url: server_b_url,
                    key_pair: (
                        shared_kid,
                        Jwk {
                            alg: Some(KeyAlgorithm::HS512),
                            decoding_key: server_b_decode,
                        },
                    ),
                    discovered_issuer: "https://auth-b.example.com".to_string(),
                },
            ],
        );

        // Server A verifies the signature but its discovered issuer
        // (`auth-a`) does not match the token's `iss` (`auth-b`); server B's key
        // fails the signature. So the token is rejected overall.
        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("does not match the issuer of the server that signed it"))
                .then_some(())
                .ok_or("Expected issuer-mismatch warning from the signing server".to_string())
        });
    }

    #[tokio::test]
    async fn it_validates_token_against_its_own_servers_issuer_with_multiple_servers() {
        use serde_json::json;

        // Two servers with distinct keys and issuers. A token signed by server
        // B and claiming server B's issuer must validate, even though server A
        // is also configured.
        let kid_a = "kid-a".to_string();
        let kid_b = "kid-b".to_string();
        let (_server_a_encode, server_a_decode) = create_key("DEADBEEF");
        let (server_b_encode, server_b_decode) = create_key("CAFED00D");

        let server_a_url = Url::from_str("https://auth-a.example.com").expect("valid server A URL");
        let server_b_url = Url::from_str("https://auth-b.example.com").expect("valid server B URL");

        let in_the_future = chrono::Utc::now().timestamp() + 1000;

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(kid_b.clone());
            h
        };
        let claims = json!({
            "aud": "test-audience",
            "iss": "https://auth-b.example.com",
            "exp": in_the_future,
            "sub": "test user"
        });
        let token = encode(&header, &claims, &server_b_encode).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let test_validator = TestTokenValidator::new(
            vec!["test-audience".to_string()],
            vec![
                "https://auth-a.example.com".to_string(),
                "https://auth-b.example.com".to_string(),
            ],
            false,
            vec![
                TestServer {
                    url: server_a_url,
                    key_pair: (
                        kid_a,
                        Jwk {
                            alg: Some(KeyAlgorithm::HS512),
                            decoding_key: server_a_decode,
                        },
                    ),
                    discovered_issuer: "https://auth-a.example.com".to_string(),
                },
                TestServer {
                    url: server_b_url,
                    key_pair: (
                        kid_b,
                        Jwk {
                            alg: Some(KeyAlgorithm::HS512),
                            decoding_key: server_b_decode,
                        },
                    ),
                    discovered_issuer: "https://auth-b.example.com".to_string(),
                },
            ],
        );

        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .token
                .token(),
            token
        );
    }

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_jwk_with_no_algorithm() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: None,
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, audience.clone(), in_the_future);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![], false, (key_id, jwk), server);

        assert_eq!(test_validator.validate(jwt).await, None);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("no algorithm specified"))
                .then_some(())
                .ok_or("Expected warning for missing algorithm".to_string())
        });
    }

    #[tokio::test]
    async fn it_rejects_when_no_server_serves_the_kid() {
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        // The token's `kid` does not match any key the configured server serves.
        let jwt = create_jwt(
            "token-kid".to_string(),
            encode_key,
            audience.clone(),
            in_the_future,
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        // The server only serves `server-kid`, so key resolution finds nothing.
        let test_validator = TestTokenValidator::single(
            vec![audience],
            vec![],
            false,
            ("server-kid".to_string(), jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);
    }

    /// Build and validate a JWT with the given claims JSON, returning the
    /// extracted scopes from the resulting `ValidToken`.
    async fn validate_with_claims(claims: serde_json::Value) -> Vec<String> {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id.clone());
            h
        };
        let token = encode(&header, &claims, &encode_key).expect("encode JWT");
        let jwt = Authorization::bearer(&token).expect("create bearer token");

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec!["test-audience".to_string()],
            vec![],
            false,
            (key_id, jwk),
            server,
        );

        test_validator
            .validate(jwt)
            .await
            .expect("valid token")
            .scopes
    }

    #[tokio::test]
    async fn it_extracts_scopes_from_scope_claim() {
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let scopes = validate_with_claims(serde_json::json!({
            "aud": "test-audience",
            "exp": in_the_future,
            "sub": "test user",
            "scope": "read write"
        }))
        .await;
        assert_eq!(scopes, vec!["read".to_string(), "write".to_string()]);
    }

    #[tokio::test]
    async fn it_extracts_scopes_from_scp_array_claim() {
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let scopes = validate_with_claims(serde_json::json!({
            "aud": "test-audience",
            "exp": in_the_future,
            "sub": "test user",
            "scp": ["read", "write"]
        }))
        .await;
        assert_eq!(scopes, vec!["read".to_string(), "write".to_string()]);
    }

    #[tokio::test]
    async fn it_extracts_scopes_from_scp_string_claim() {
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let scopes = validate_with_claims(serde_json::json!({
            "aud": "test-audience",
            "exp": in_the_future,
            "sub": "test user",
            "scp": "read write"
        }))
        .await;
        assert_eq!(scopes, vec!["read".to_string(), "write".to_string()]);
    }

    #[tokio::test]
    async fn it_prefers_scope_over_scp_when_both_present() {
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let scopes = validate_with_claims(serde_json::json!({
            "aud": "test-audience",
            "exp": in_the_future,
            "sub": "test user",
            "scope": "read",
            "scp": ["write"]
        }))
        .await;
        assert_eq!(scopes, vec!["read".to_string()]);
    }

    #[tokio::test]
    async fn it_returns_empty_scopes_when_neither_claim_present() {
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let scopes = validate_with_claims(serde_json::json!({
            "aud": "test-audience",
            "exp": in_the_future,
            "sub": "test user"
        }))
        .await;
        assert!(scopes.is_empty());
    }
}
