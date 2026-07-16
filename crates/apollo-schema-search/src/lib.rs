//! Hybrid (lexical + semantic) search over GraphQL schema operations.
mod embedder;
mod fastembed_embedder;
mod fusion;
mod hybrid;
mod vector_search;
mod vector_store;

pub use embedder::{EmbedError, Embedder, FakeEmbedder};
pub use fastembed_embedder::FastembedEmbedder;
pub use fusion::rrf_fuse;
pub use hybrid::HybridSearch;
pub use vector_search::VectorSearch;
pub use vector_store::{InMemoryVectorStore, VectorStore};
