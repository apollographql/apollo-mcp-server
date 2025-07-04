use tantivy::TantivyError;

/// An error during indexing
#[derive(Debug, thiserror::Error)]
pub enum IndexingError {
    #[error("Unable to index schema: {0}")]
    TantivyError(#[from] TantivyError),
}

/// An error in a search operation
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("Search error: {0}")]
    TantivyError(#[from] TantivyError),
}
