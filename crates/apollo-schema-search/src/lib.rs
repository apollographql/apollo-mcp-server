//! Hybrid (lexical + semantic) search over GraphQL schema operations.
mod embedder;
mod fusion;
mod vector_store;

pub use embedder::{EmbedError, Embedder, FakeEmbedder};
pub use fusion::rrf_fuse;
pub use vector_store::{InMemoryVectorStore, VectorStore};
