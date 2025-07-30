use std::ops::Deref;

use headers::{Authorization, authorization::Bearer};

/// A validated authentication token
///
/// Note: This is used as a marker to ensure that we have validated this
/// separately from just reading the header itself.
#[derive(Clone)]
pub(crate) struct ValidToken(pub(super) Authorization<Bearer>);

impl Deref for ValidToken {
    type Target = Authorization<Bearer>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
