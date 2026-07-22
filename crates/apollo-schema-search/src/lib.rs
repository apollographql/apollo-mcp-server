//! Hybrid (lexical + semantic) search over GraphQL schema operations.
mod embedder;
mod embedding_store;
mod fastembed_embedder;
mod fusion;
mod hybrid;
mod postgres_cache;
mod vector_search;
mod vector_store;

pub use embedder::{EmbedError, Embedder, FakeEmbedder};
pub use embedding_store::{CacheError, DOC_BUILDER_VERSION, EmbeddingStore, doc_key};
pub use fastembed_embedder::FastembedEmbedder;
pub use fusion::rrf_fuse;
pub use hybrid::HybridSearch;
pub use postgres_cache::PostgresCache;
pub use vector_search::VectorSearch;
pub use vector_store::{InMemoryVectorStore, VectorStore};
