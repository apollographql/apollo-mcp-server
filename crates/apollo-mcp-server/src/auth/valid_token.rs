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

/// Resolves a signing key for a `(server, key_id)` pair, returning it together
/// with the issuer that server advertises in discovery.
///
/// This is the single seam that production fetches over the network and tests
/// substitute in memory. Everything else about validation lives in
/// [`TokenValidator`], so adding a new validation input does not touch this
/// trait or its implementations.
pub(super) trait KeyResolver {
    /// Fetch the signing key for `key_id`, together with the issuer the server
    /// advertises in its discovery metadata. Returns `None` when the server
    /// does not serve a key with that `key_id`.
    ///
    /// The returned issuer is the value issuer validation binds to: a token's
    /// `iss` claim must equal it, so a token signed by one configured server
    /// cannot pass by claiming a different server's issuer.
    async fn resolve_key(&self, server: &Url, key_id: &str) -> Option<(Jwk, String)>;
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
        let jwt = token.token();
        let header = decode_header(jwt).ok()?;
        let key_id = header.kid.as_ref()?;

        // Cheap pre-network gate: drop tokens whose unverified `iss` / `aud`
        // claims cannot possibly satisfy this deployment's configuration before
        // calling [`KeyResolver::resolve_key`], which is the only outbound
        // network call on this path. See [`Self::unverified_claims_could_match`].
        if !self.unverified_claims_could_match(jwt) {
            return None;
        }

        for server in self.servers {
            let Some((jwk, discovered_issuer)) = self.keys.resolve_key(server, key_id).await else {
                continue;
            };

            let validation = {
                let Some(alg) = jwk.alg else {
                    warn!("Skipping JWK with no algorithm specified");
                    continue;
                };
                let Some(algorithm) = jwt_algorithm(alg) else {
                    warn!("Skipping JWT signed by unsupported algorithm: {alg:?}");
                    continue;
                };
                // The pre-network gate (`unverified_claims_could_match`) is
                // the sole owner of the `iss` / `aud` allowlist rule, so we
                // deliberately disable `jsonwebtoken`'s own iss/aud checks
                // here. Keeping the rule in one place prevents the two sides
                // from drifting apart over time.
                let mut val = Validation::new(algorithm);
                val.validate_aud = false;
                val
            };

            match decode::<Claims>(jwt, &jwk.decoding_key, &validation) {
                Ok(token_data) => {
                    // Bind the token's `iss` to the issuer the signing server
                    // advertises in its discovery metadata. This is the one
                    // iss/aud check the pre-network gate cannot perform,
                    // since it depends on which server's JWKS verified the
                    // signature. Without it, a token signed by one configured
                    // server could pass while claiming a different configured
                    // server's issuer.
                    if !self.issuers.is_empty() {
                        // Defensive: the pre-network gate already requires
                        // `iss` when issuers are configured, so this should
                        // not fire in practice.
                        let Some(token_issuer) = token_data.claims.iss.as_deref() else {
                            warn!("Token is missing the required `iss` claim");
                            break;
                        };
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

    /// Returns `false` when the JWT payload cannot satisfy this deployment's
    /// `iss` / `aud` configuration. This is the sole owner of that
    /// allowlist rule: [`Self::validate`] deliberately disables
    /// `jsonwebtoken`'s own iss/aud checks so the rule lives in exactly one
    /// place.
    ///
    /// Safety of reading claims before the signature is verified: a
    /// successful signature check proves the payload bytes were not
    /// tampered with after signing — it cannot change a claim's value. So
    /// matching `iss` / `aud` against the configured allowlist before the
    /// signature is checked yields the same answer as matching after,
    /// against byte-identical data. The result is only ever used to
    /// *reject* a token; nothing here can authorize one.
    fn unverified_claims_could_match(&self, jwt: &str) -> bool {
        let Some(claims) = decode_unverified_payload(jwt) else {
            // Not a structurally valid JWT, or the payload isn't JSON — the
            // resolver/decoder would reject it anyway, so short-circuit.
            warn!("Token payload could not be decoded for pre-network claim check");
            return false;
        };

        if !self.issuers.is_empty() {
            let Some(iss) = claims.iss.as_deref() else {
                warn!("Token is missing the required `iss` claim");
                return false;
            };
            if !self.issuers.iter().any(|configured| configured == iss) {
                warn!(
                    token_issuer = %iss,
                    "Token `iss` does not match any configured issuer"
                );
                return false;
            }
        }

        if !self.allow_any_audience {
            if claims.aud.is_empty() {
                warn!("Token is missing the required `aud` claim");
                return false;
            }
            let matches_audience = claims
                .aud
                .iter()
                .any(|a| self.audiences.iter().any(|configured| configured == a));
            if !matches_audience {
                warn!(
                    token_audiences = ?claims.aud,
                    "Token `aud` does not match any configured audience"
                );
                return false;
            }
        }

        true
    }
}

/// Claims which must be present in the JWT (and must match validation) in order
/// for a JWT to be considered valid.
///
/// See: https://auth0.com/docs/secure/tokens/json-web-tokens/json-web-token-claims#registered-claims
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Claims {
    /// The intended audience of this token.
    /// Can be either a single string or an array of strings per JWT spec. (https://datatracker.ietf.org/doc/html/rfc7519#section-4.1.3)
    /// Some providers (e.g., AWS Cognito) omit `aud` entirely in access tokens,
    /// so this field defaults to an empty vec when absent.
    #[serde(default, deserialize_with = "deserialize_audience")]
    aud: Vec<String>,

    /// The issuer of this token (`iss`). Optional in the struct so tokens
    /// without it still deserialize; enforced in `validate` when issuers are configured.
    #[serde(default)]
    iss: Option<String>,

    /// The subject the token was issued for. Required so a token without a
    /// `sub` claim is rejected at deserialize time.
    sub: String,

    /// OAuth scope claim (space-separated list per RFC 6749)
    #[serde(default)]
    scope: Option<String>,

    /// Non-standard scope claim. Okta emits this as an array of
    /// strings; Microsoft Entra emits it as a space-separated string.
    /// Used as a fallback when the RFC 9068 `scope` claim is absent.
    #[serde(default)]
    scp: Option<ScpClaim>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ScpClaim {
    Array(Vec<String>),
    String(String),
}

impl Claims {
    /// Resolve the scopes granted by this token, preferring the RFC 9068
    /// `scope` claim and falling back to the `scp` claim used by Okta (array)
    /// and Microsoft Entra (space-separated string).
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

/// Subset of [`Claims`] used for the pre-network gate in
/// [`TokenValidator::unverified_claims_could_match`]: only the fields needed
/// to decide whether the token could ever satisfy the configured `iss` / `aud`.
///
/// This struct is only ever populated from the *unverified* JWT payload, so
/// values must be used to reject tokens — never to authorize them. The fields
/// match [`Claims`] so the deserialization rules stay consistent across the
/// pre- and post-verify checks.
#[derive(Debug, Deserialize)]
struct UnverifiedClaims {
    #[serde(default)]
    iss: Option<String>,

    #[serde(default, deserialize_with = "deserialize_audience")]
    aud: Vec<String>,
}

/// Decode just the payload of a JWT (the middle of the three `.`-separated
/// segments) without verifying the signature.
///
/// A base64-url decode and a `serde_json::from_slice` into a two-field struct,
/// nothing else. The work is bounded by the input token size (which `axum`
/// caps via the `Authorization` header limit), so this adds at most a few
/// microseconds of local CPU per request and replaces the uncached outbound
/// HTTP calls that previously fired before the same rejection decision
/// (SECOPS-6447).
///
/// Returns `None` for anything that isn't a structurally valid JWT or whose
/// payload isn't valid JSON. Callers must use the returned claims only to
/// *reject* tokens — see [`UnverifiedClaims`] and
/// [`TokenValidator::unverified_claims_could_match`].
fn decode_unverified_payload(jwt: &str) -> Option<UnverifiedClaims> {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    // RFC 7519: a JWT is exactly three base64url segments joined by `.`.
    // We only need the middle segment; reject anything with a different shape
    // so this helper can never silently accept malformed input.
    let mut parts = jwt.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    serde_json::from_slice(&payload).ok()
}

/// Accepts the JWT `aud` claim as either a single string or an array of
/// strings, normalizing both to a `Vec<String>` (empty when absent).
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

/// Maps a JWKS key algorithm to the `jsonwebtoken` [`Algorithm`] used for
/// verification, returning `None` for algorithms this library does not support.
fn jwt_algorithm(alg: jwk::KeyAlgorithm) -> Option<Algorithm> {
    Some(match alg {
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
        _ => return None,
    })
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use headers::{Authorization, authorization::Bearer};
    use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode, jwk::KeyAlgorithm};
    use jwks::Jwk;
    use serde::Serialize;
    use tracing_test::traced_test;
    use url::Url;

    use super::{Claims, KeyResolver, TokenValidator, ValidToken, jwt_algorithm};

    /// A single upstream server in the stub resolver: its URL, the one
    /// `(kid, jwk)` it serves, and the issuer it advertises in discovery.
    struct TestServer {
        url: Url,
        key_pair: (String, Jwk),
        discovered_issuer: String,
    }

    /// In-memory [`KeyResolver`] that stands in for the network in tests.
    ///
    /// Counts calls to [`KeyResolver::resolve_key`] so tests can assert that
    /// the pre-network gate in [`TokenValidator::validate`] never hits the
    /// network seam (SECOPS-6447 acceptance criterion).
    struct StubKeyResolver<'a> {
        servers: &'a [TestServer],
        resolve_calls: Arc<AtomicUsize>,
    }

    impl KeyResolver for StubKeyResolver<'_> {
        async fn resolve_key(&self, server: &Url, key_id: &str) -> Option<(Jwk, String)> {
            self.resolve_calls.fetch_add(1, Ordering::SeqCst);
            // Find the requested server, then return its key only if the `kid` matches.
            let test_server = self.servers.iter().find(|s| &s.url == server)?;
            test_server.key_pair.0.eq(key_id).then(|| {
                (
                    test_server.key_pair.1.clone(),
                    test_server.discovered_issuer.clone(),
                )
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
        /// Shared with [`StubKeyResolver`] so [`Self::resolve_calls`] reflects
        /// every call made during [`Self::validate`].
        resolve_calls: Arc<AtomicUsize>,
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
                resolve_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// Number of times the underlying [`KeyResolver`] was invoked during
        /// the most recent (or all preceding) calls to [`Self::validate`].
        fn resolve_calls(&self) -> usize {
            self.resolve_calls.load(Ordering::SeqCst)
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
                    resolve_calls: Arc::clone(&self.resolve_calls),
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

        // Rejection now happens in the pre-network gate (SECOPS-6447 / AMS-529);
        // the warning identifies that path rather than the post-verify
        // `InvalidAudience` message the older flow emitted.
        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("does not match any configured audience"))
                .then_some(())
                .ok_or("Expected warning for aud mismatch".to_string())
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

        // Rejection happens in the pre-network gate (SECOPS-6447 / AMS-529);
        // earlier code paths emitted `InvalidAudience` from the post-verify
        // step, which is no longer reached for this case.
        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("does not match any configured audience"))
                .then_some(())
                .ok_or("Expected warning for aud mismatch".to_string())
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

        // Rejection happens in the pre-network gate (SECOPS-6447 / AMS-529);
        // the post-verify `InvalidIssuer` path is no longer reached for this
        // case.
        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("does not match any configured issuer"))
                .then_some(())
                .ok_or("Expected warning for iss mismatch".to_string())
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

    #[traced_test]
    #[tokio::test]
    async fn it_rejects_jwk_with_unsupported_algorithm() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        // The JWK advertises a key-management algorithm the validator does not
        // support for token signing, so `jwt_algorithm` returns `None`.
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::RSA1_5),
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
                .any(|line| line.contains("unsupported algorithm"))
                .then_some(())
                .ok_or("Expected warning for unsupported algorithm".to_string())
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

    // --- Pre-network claim gate (SECOPS-6447) ------------------------------
    //
    // The post-verify checks above run *after* the network call to resolve a
    // signing key. These tests exercise the cheap pre-network gate added in
    // AMS-529: tokens whose unverified `iss` / `aud` claims cannot satisfy the
    // configured allowlists must be rejected with **zero** calls to the
    // [`KeyResolver`] seam.
    //
    // Each test asserts both the rejection *and*
    // `test_validator.resolve_calls() == 0` so a regression that loses the
    // pre-network short-circuit is caught directly.

    /// Builds a JWT with arbitrary claims (matching `Algorithm::HS512`); used
    /// by the pre-network gate tests to vary `iss` / `aud` independently.
    fn create_jwt_with_claims(
        key_id: String,
        key: EncodingKey,
        claims: serde_json::Value,
    ) -> Authorization<Bearer> {
        let header = {
            let mut h = Header::new(Algorithm::HS512);
            h.kid = Some(key_id);
            h
        };
        let token = encode(&header, &claims, &key).expect("encode JWT");
        Authorization::bearer(&token).expect("create bearer token")
    }

    /// Matching `iss` and `aud` must pass the pre-network gate, so the
    /// resolver is still consulted as part of normal validation.
    #[tokio::test]
    async fn pre_check_passes_when_iss_and_aud_match() {
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
        let jwt = create_jwt_with_claims(
            key_id.clone(),
            encode_key,
            json!({
                "aud": audience,
                "iss": issuer,
                "exp": in_the_future,
                "sub": "test user",
            }),
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![audience], vec![issuer], false, (key_id, jwk), server);

        assert!(test_validator.validate(jwt).await.is_some());
        assert_eq!(
            test_validator.resolve_calls(),
            1,
            "pre-check should pass through to the resolver"
        );
    }

    /// `iss` not in the configured allowlist must be rejected before the
    /// resolver is consulted — this is the core SECOPS-6447 acceptance check.
    #[traced_test]
    #[tokio::test]
    async fn pre_check_rejects_mismatched_iss_without_calling_resolver() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt_with_claims(
            key_id.clone(),
            encode_key,
            json!({
                "aud": "test-audience",
                "iss": "https://evil.example.com",
                "exp": in_the_future,
                "sub": "test user",
            }),
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec!["test-audience".to_string()],
            vec!["https://auth.example.com".to_string()],
            false,
            (key_id, jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);
        assert_eq!(
            test_validator.resolve_calls(),
            0,
            "rejected token must not reach the network seam"
        );

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("does not match any configured issuer"))
                .then_some(())
                .ok_or("Expected warning for iss mismatch".to_string())
        });
    }

    /// `aud` not in the configured allowlist must be rejected before the
    /// resolver is consulted.
    #[traced_test]
    #[tokio::test]
    async fn pre_check_rejects_mismatched_aud_without_calling_resolver() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt_with_claims(
            key_id.clone(),
            encode_key,
            json!({
                "aud": "wrong-audience",
                "iss": "https://auth.example.com",
                "exp": in_the_future,
                "sub": "test user",
            }),
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec!["expected-audience".to_string()],
            vec![],
            false,
            (key_id, jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);
        assert_eq!(
            test_validator.resolve_calls(),
            0,
            "rejected token must not reach the network seam"
        );

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("does not match any configured audience"))
                .then_some(())
                .ok_or("Expected warning for aud mismatch".to_string())
        });
    }

    /// A multi-valued `aud` (array) where none of the values match the
    /// configured audiences must also be rejected pre-network.
    #[tokio::test]
    async fn pre_check_rejects_array_aud_with_no_matches_without_calling_resolver() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt_with_claims(
            key_id.clone(),
            encode_key,
            json!({
                "aud": ["wrong-1", "wrong-2"],
                "exp": in_the_future,
                "sub": "test user",
            }),
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec!["expected-audience".to_string()],
            vec![],
            false,
            (key_id, jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);
        assert_eq!(test_validator.resolve_calls(), 0);
    }

    /// Missing `iss` when issuers are configured must be rejected pre-network,
    /// matching the post-verify "missing required `iss` claim" behaviour.
    #[traced_test]
    #[tokio::test]
    async fn pre_check_rejects_missing_iss_when_issuers_configured_without_calling_resolver() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt_with_claims(
            key_id.clone(),
            encode_key,
            // No `iss` claim.
            json!({
                "aud": "test-audience",
                "exp": in_the_future,
                "sub": "test user",
            }),
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec!["test-audience".to_string()],
            vec!["https://auth.example.com".to_string()],
            false,
            (key_id, jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);
        assert_eq!(test_validator.resolve_calls(), 0);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("missing the required `iss` claim"))
                .then_some(())
                .ok_or("Expected warning for missing iss claim".to_string())
        });
    }

    /// Missing `aud` when audience validation is required must be rejected
    /// pre-network. Mirrors the post-verify check on line ~102 of `validate`.
    #[traced_test]
    #[tokio::test]
    async fn pre_check_rejects_missing_aud_when_audience_required_without_calling_resolver() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt_with_claims(
            key_id.clone(),
            encode_key,
            // No `aud` claim.
            json!({
                "exp": in_the_future,
                "sub": "test user",
            }),
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec!["expected-audience".to_string()],
            vec![],
            false,
            (key_id, jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);
        assert_eq!(test_validator.resolve_calls(), 0);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("missing the required `aud` claim"))
                .then_some(())
                .ok_or("Expected warning for missing aud claim".to_string())
        });
    }

    /// With `allow_any_audience` and no configured issuers, the pre-check is a
    /// no-op: even tokens with bogus `iss` / `aud` proceed to the resolver.
    /// Guards against the pre-check accidentally tightening defaults.
    #[tokio::test]
    async fn pre_check_is_noop_when_no_issuers_and_allow_any_audience() {
        use serde_json::json;

        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt_with_claims(
            key_id.clone(),
            encode_key,
            json!({
                "aud": "anything",
                "iss": "https://anyone.example.com",
                "exp": in_the_future,
                "sub": "test user",
            }),
        );

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator =
            TestTokenValidator::single(vec![], vec![], true, (key_id, jwk), server);

        assert!(test_validator.validate(jwt).await.is_some());
        assert_eq!(
            test_validator.resolve_calls(),
            1,
            "no-op pre-check must still consult the resolver"
        );
    }

    /// A token that isn't structurally a JWT is dropped pre-network — without
    /// it, the resolver would still never succeed, but it would be invoked
    /// once for nothing.
    #[traced_test]
    #[tokio::test]
    async fn pre_check_rejects_malformed_payload_without_calling_resolver() {
        // Build a token whose header parses (kid present) but whose payload is
        // not valid JSON. Hand-rolled so we keep control of what's well-formed.
        // The header below is `{"alg":"HS512","kid":"some-example-id","typ":"JWT"}`.
        let header_b64 = "eyJhbGciOiJIUzUxMiIsImtpZCI6InNvbWUtZXhhbXBsZS1pZCIsInR5cCI6IkpXVCJ9";
        // Payload bytes that decode to the ASCII string "not-json".
        let payload_b64 = "bm90LWpzb24";
        let signature_b64 = "AAAA";
        let token_str = format!("{header_b64}.{payload_b64}.{signature_b64}");
        let jwt = Authorization::bearer(&token_str).expect("create bearer token");

        let (_, decode_key) = create_key("DEADBEEF");
        let jwk = Jwk {
            alg: Some(KeyAlgorithm::HS512),
            decoding_key: decode_key,
        };

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator::single(
            vec!["test-audience".to_string()],
            vec!["https://auth.example.com".to_string()],
            false,
            ("some-example-id".to_string(), jwk),
            server,
        );

        assert_eq!(test_validator.validate(jwt).await, None);
        assert_eq!(test_validator.resolve_calls(), 0);

        logs_assert(|lines: &[&str]| {
            lines
                .iter()
                .filter(|line| line.contains("WARN"))
                .any(|line| line.contains("could not be decoded for pre-network claim check"))
                .then_some(())
                .ok_or("Expected warning for undecodable payload".to_string())
        });
    }

    /// Deserialize [`Claims`] from JSON and resolve its scopes. Exercises the
    /// `scope`/`scp` claim handling directly, without the JWT round-trip.
    fn scopes_of(claims: serde_json::Value) -> Vec<String> {
        serde_json::from_value::<Claims>(claims)
            .expect("deserialize claims")
            .scopes()
    }

    #[test]
    fn scopes_come_from_scope_claim() {
        let scopes = scopes_of(serde_json::json!({ "sub": "u", "scope": "read write" }));
        assert_eq!(scopes, vec!["read".to_string(), "write".to_string()]);
    }

    #[test]
    fn scopes_fall_back_to_scp_array() {
        let scopes = scopes_of(serde_json::json!({ "sub": "u", "scp": ["read", "write"] }));
        assert_eq!(scopes, vec!["read".to_string(), "write".to_string()]);
    }

    #[test]
    fn scopes_fall_back_to_scp_string() {
        let scopes = scopes_of(serde_json::json!({ "sub": "u", "scp": "read write" }));
        assert_eq!(scopes, vec!["read".to_string(), "write".to_string()]);
    }

    #[test]
    fn scope_claim_wins_over_scp() {
        let scopes =
            scopes_of(serde_json::json!({ "sub": "u", "scope": "read", "scp": ["write"] }));
        assert_eq!(scopes, vec!["read".to_string()]);
    }

    #[test]
    fn scopes_empty_when_neither_claim_present() {
        let scopes = scopes_of(serde_json::json!({ "sub": "u" }));
        assert!(scopes.is_empty());
    }

    #[test]
    fn null_audience_deserializes_to_empty() {
        let claims: Claims = serde_json::from_value(serde_json::json!({ "sub": "u", "aud": null }))
            .expect("deserialize claims");
        assert!(claims.aud.is_empty());
    }

    #[test]
    fn jwt_algorithm_maps_every_supported_algorithm() {
        use KeyAlgorithm::*;
        let supported = [
            (HS256, Algorithm::HS256),
            (HS384, Algorithm::HS384),
            (HS512, Algorithm::HS512),
            (ES256, Algorithm::ES256),
            (ES384, Algorithm::ES384),
            (RS256, Algorithm::RS256),
            (RS384, Algorithm::RS384),
            (RS512, Algorithm::RS512),
            (PS256, Algorithm::PS256),
            (PS384, Algorithm::PS384),
            (PS512, Algorithm::PS512),
            (EdDSA, Algorithm::EdDSA),
        ];
        for (key_alg, expected) in supported {
            assert_eq!(jwt_algorithm(key_alg), Some(expected), "{key_alg:?}");
        }
    }

    #[test]
    fn jwt_algorithm_returns_none_for_unsupported() {
        // `RSA1_5` is a key-management algorithm, not a token-signing one.
        assert_eq!(jwt_algorithm(KeyAlgorithm::RSA1_5), None);
    }
}
