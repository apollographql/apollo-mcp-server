//! OCI image manifest loader.
//!
//! Pulls an OCI image, extracts each layer into a temporary directory, locates
//! the manifest file via the image's `org.apollographql.mcp.manifest`
//! annotation, then parses it like a local manifest. The returned `TempDir`
//! must be kept alive for the lifetime of the running server so the
//! schema/operation files referenced by the manifest remain on disk.

use std::path::PathBuf;

use oci_client::{
    Client, Reference,
    client::{ClientConfig, ClientProtocol},
    manifest::{IMAGE_LAYER_GZIP_MEDIA_TYPE, IMAGE_LAYER_MEDIA_TYPE},
    secrets::RegistryAuth,
};

use super::local::{LocalLoadError, load_local};
use super::types::Manifest;

/// Annotation key carrying the manifest filename inside the image.
pub const MANIFEST_ANNOTATION: &str = "org.apollographql.mcp.manifest";

/// Custom media type a publisher MAY use for the bundle tarball layer.
pub const LAYER_MEDIA_TYPE: &str = "application/vnd.apollographql.mcp.bundle.v1+tar";

#[derive(Debug, thiserror::Error)]
pub enum OciLoadError {
    #[error("invalid OCI image reference '{image}': {source}")]
    BadReference {
        image: String,
        #[source]
        source: oci_client::ParseError,
    },
    #[error("failed to pull OCI image '{image}': {source}")]
    Pull {
        image: String,
        #[source]
        source: oci_client::errors::OciDistributionError,
    },
    #[error("image '{image}' is missing the {MANIFEST_ANNOTATION} annotation")]
    MissingAnnotation { image: String },
    #[error("failed to extract bundle for image '{image}': {source}")]
    Extract {
        image: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Local(#[from] LocalLoadError),
}

/// Pull `image`, extract its layers into a temp dir, read the manifest the
/// image's annotation points to, and parse it like a local manifest.
///
/// The returned `TempDir` owns the on-disk extraction directory; drop it and
/// the files vanish.
pub async fn load_oci(image: &str) -> Result<(Manifest, tempfile::TempDir), OciLoadError> {
    let reference: Reference = image.parse().map_err(|source| OciLoadError::BadReference {
        image: image.to_string(),
        source,
    })?;

    let client = Client::new(ClientConfig {
        protocol: ClientProtocol::Http,
        ..ClientConfig::default()
    });
    let auth = RegistryAuth::Anonymous;

    let (image_manifest, _digest) = client
        .pull_image_manifest(&reference, &auth)
        .await
        .map_err(|source| OciLoadError::Pull {
            image: image.to_string(),
            source,
        })?;

    let manifest_filename = image_manifest
        .annotations
        .as_ref()
        .and_then(|a| a.get(MANIFEST_ANNOTATION))
        .cloned()
        .ok_or_else(|| OciLoadError::MissingAnnotation {
            image: image.to_string(),
        })?;

    let pulled = client
        .pull(
            &reference,
            &auth,
            vec![
                LAYER_MEDIA_TYPE,
                IMAGE_LAYER_MEDIA_TYPE,
                IMAGE_LAYER_GZIP_MEDIA_TYPE,
            ],
        )
        .await
        .map_err(|source| OciLoadError::Pull {
            image: image.to_string(),
            source,
        })?;

    let tmp = tempfile::tempdir().map_err(|source| OciLoadError::Extract {
        image: image.to_string(),
        source,
    })?;

    for layer in pulled.layers {
        let cursor = std::io::Cursor::new(layer.data);
        let mut archive = tar::Archive::new(cursor);
        archive
            .unpack(tmp.path())
            .map_err(|source| OciLoadError::Extract {
                image: image.to_string(),
                source,
            })?;
    }

    let manifest_path: PathBuf = tmp.path().join(manifest_filename);
    let manifest = load_local(&manifest_path)?;
    Ok((manifest, tmp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_rejects_bad_image_reference() {
        // tokio runtime not needed: parse failure is synchronous.
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(load_oci("this is not a valid:ref:at:all"));
        assert!(matches!(result, Err(OciLoadError::BadReference { .. })));
    }
}
