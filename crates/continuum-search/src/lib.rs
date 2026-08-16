//! Semantic code search.
//!
//! A static-embedding model (model2vec) plus an in-memory vector index. The
//! daemon fuses this with the graph's lexical BM25 search via reciprocal rank
//! fusion, so `search_code` returns one ranked, compact result set.

mod embedder;
mod fusion;
mod hybrid;
mod index;

pub use embedder::Embedder;
pub use fusion::fuse;
pub use hybrid::query;
pub use index::{SemanticEngine, SemanticStatus, SymbolDoc};
