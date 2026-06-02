use oci_client::{
    Client, Reference,
    client::{ClientConfig, ClientProtocol},
    secrets::RegistryAuth,
};

#[derive(Debug, thiserror::Error)]
pub enum SchemaOciError {
    #[error("invalid OCI reference '{reference}': {source}")]
    BadReference {
        reference: String,
        #[source]
        source: oci_client::ParseError,
    },
    #[error("failed to pull schema from '{reference}': {source}")]
    Pull {
        reference: String,
        #[source]
        source: oci_client::errors::OciDistributionError,
    },
    #[error("schema artifact '{reference}' has no layers")]
    NoLayers { reference: String },
    #[error("schema layer is not valid UTF-8: {source}")]
    Utf8 {
        #[source]
        source: std::string::FromUtf8Error,
    },
}

/// Fetch a schema SDL stored as a plain-text OCI artifact in Zot.
///
/// The artifact is expected to have exactly one layer containing the raw SDL
/// bytes, pushed by ci-build via:
///   `oras push <registry>/schemas/<name>:<sha> --plain-http schema.graphql:text/plain`
pub async fn fetch_schema_text(image_ref: &str) -> Result<String, SchemaOciError> {
    let reference: Reference =
        image_ref
            .parse()
            .map_err(|source| SchemaOciError::BadReference {
                reference: image_ref.to_string(),
                source,
            })?;

    let client = Client::new(ClientConfig {
        protocol: ClientProtocol::Http,
        ..ClientConfig::default()
    });

    let pulled = client
        .pull(
            &reference,
            &RegistryAuth::Anonymous,
            vec!["text/plain", "application/octet-stream"],
        )
        .await
        .map_err(|source| SchemaOciError::Pull {
            reference: image_ref.to_string(),
            source,
        })?;

    let layer = pulled
        .layers
        .into_iter()
        .next()
        .ok_or_else(|| SchemaOciError::NoLayers {
            reference: image_ref.to_string(),
        })?;

    String::from_utf8(layer.data.to_vec()).map_err(|source| SchemaOciError::Utf8 { source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_rejects_a_bad_reference() {
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fetch_schema_text("not a valid ref at all"));
        assert!(matches!(result, Err(SchemaOciError::BadReference { .. })));
    }
}
