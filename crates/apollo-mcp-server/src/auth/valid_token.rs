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
pub(crate) struct ValidToken(pub(super) Authorization<Bearer>);

impl Deref for ValidToken {
    type Target = Authorization<Bearer>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Trait to handle validation of tokens
pub(super) trait ValidateToken {
    /// Get the intended audiences
    fn get_audiences(&self) -> &Vec<String>;

    /// Get the available upstream servers
    fn get_servers(&self) -> &Vec<Url>;

    /// Fetch the key by its ID
    async fn get_key(&self, server: &Url, key_id: &str) -> Option<Jwk>;

    /// Attempt to validate a token against the validator
    async fn validate(&self, token: Authorization<Bearer>) -> Option<ValidToken> {
        /// Claims which must be present in the JWT (and must match validation)
        /// in order for a JWT to be considered valid.
        ///
        /// See: https://auth0.com/docs/secure/tokens/json-web-tokens/json-web-token-claims#registered-claims
        #[derive(Clone, Debug, Serialize, Deserialize)]
        pub struct Claims {
            /// The intended audience of this token.
            pub aud: String,

            /// The user who owns this token
            pub sub: String,
        }

        let jwt = token.token();
        let header = decode_header(jwt).ok()?;
        let key_id = header.kid.as_ref()?;

        for server in self.get_servers() {
            let Some(jwk) = self.get_key(server, key_id).await else {
                continue;
            };

            let validation = {
                let mut val = Validation::new(match jwk.alg {
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
                        warn!("Skipping JWT signed by unsupported algorithm: {other}");
                        continue;
                    }
                });
                val.set_audience(self.get_audiences());

                val
            };

            match decode::<Claims>(jwt, &jwk.decoding_key, &validation) {
                Ok(_) => {
                    return Some(ValidToken(token));
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
    use url::Url;

    use super::ValidateToken;

    struct TestTokenValidator {
        audiences: Vec<String>,
        key_pair: (String, Jwk),
        servers: Vec<Url>,
    }

    impl ValidateToken for TestTokenValidator {
        fn get_audiences(&self) -> &Vec<String> {
            &self.audiences
        }

        fn get_servers(&self) -> &Vec<url::Url> {
            &self.servers
        }

        async fn get_key(&self, server: &url::Url, key_id: &str) -> Option<jwks::Jwk> {
            // Return nothing if the server is not known to us
            if !self.get_servers().contains(server) {
                return None;
            }

            // Only return the key if it is the one we know
            self.key_pair
                .0
                .eq(key_id)
                .then_some(self.key_pair.1.clone())
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
            alg: KeyAlgorithm::HS512,
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_future = chrono::Utc::now().timestamp() + 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, audience.clone(), in_the_future);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator {
            audiences: vec![audience],
            key_pair: (key_id, jwk),
            servers: vec![server],
        };

        let token = jwt.token().to_string();
        assert_eq!(
            test_validator
                .validate(jwt)
                .await
                .expect("valid token")
                .0
                .token(),
            token
        );
    }

    #[tokio::test]
    async fn it_rejects_different_key() {
        let key_id = "some-example-id".to_string();
        let (_, decode_key) = create_key("CAFED00D");
        let (bad_encode_key, _) = create_key("DEADC0DE");
        let jwk = Jwk {
            alg: KeyAlgorithm::HS512,
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

        let test_validator = TestTokenValidator {
            audiences: vec![audience],
            key_pair: (key_id, jwk),
            servers: vec![server],
        };

        assert_eq!(test_validator.validate(jwt).await, None);
    }

    #[tokio::test]
    async fn it_rejects_expired() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("F0CACC1A");
        let jwk = Jwk {
            alg: KeyAlgorithm::HS512,
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let in_the_past = chrono::Utc::now().timestamp() - 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, audience.clone(), in_the_past);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator {
            audiences: vec![audience],
            key_pair: (key_id, jwk),
            servers: vec![server],
        };

        assert_eq!(test_validator.validate(jwt).await, None);
    }

    #[tokio::test]
    async fn it_rejects_different_audience() {
        let key_id = "some-example-id".to_string();
        let (encode_key, decode_key) = create_key("F0CACC1A");
        let jwk = Jwk {
            alg: KeyAlgorithm::HS512,
            decoding_key: decode_key,
        };

        let audience = "test-audience".to_string();
        let bad_audience = "not-test-audience".to_string();
        let in_the_past = chrono::Utc::now().timestamp() - 1000;
        let jwt = create_jwt(key_id.clone(), encode_key, bad_audience, in_the_past);

        let server =
            Url::from_str("https://auth.example.com").expect("should parse a valid example server");

        let test_validator = TestTokenValidator {
            audiences: vec![audience],
            key_pair: (key_id, jwk),
            servers: vec![server],
        };

        assert_eq!(test_validator.validate(jwt).await, None);
    }
}
