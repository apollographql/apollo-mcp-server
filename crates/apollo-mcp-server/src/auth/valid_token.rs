/// A validated token string
#[derive(Clone)]
pub(crate) struct ValidToken(pub(super) String);

impl ValidToken {
    /// Read the contents of the token, consuming it.
    pub fn read(self) -> String {
        self.0
    }
}
