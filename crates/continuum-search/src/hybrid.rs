//! The hybrid search policy: when to fuse lexical + semantic results and when
//! to fall back to lexical-only.
//!
//! The lexical list is fetched by the caller (the graph owns BM25); this module
//! owns everything downstream of it — the readiness check, the semantic fetch,
//! reciprocal rank fusion, and the dormant fallback that kicks the lazy model
//! load.

use continuum_core::dto::SearchHit;

use crate::fusion::fuse;
use crate::index::{SemanticEngine, SemanticStatus};

/// Answer a search: fuse the lexical hits with semantic hits when the engine is
/// ready, otherwise truncate the lexical list and kick the lazy model load.
pub async fn query(
    engine: &SemanticEngine,
    query: &str,
    lexical: Vec<SearchHit>,
    limit: usize,
    kind: Option<&str>,
) -> Vec<SearchHit> {
    if engine.status() == SemanticStatus::Ready {
        let semantic = engine.search(query, limit * 2, kind).await;
        fuse(lexical, semantic, limit)
    } else {
        engine.kick();
        let mut hits = lexical;
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, line: usize) -> SearchHit {
        SearchHit {
            name: name.to_string(),
            kind: "function".to_string(),
            path: "f.rs".to_string(),
            line,
            signature: String::new(),
            is_test: false,
            score: 0.0,
        }
    }

    fn lexical() -> Vec<SearchHit> {
        vec![hit("a", 1), hit("b", 2), hit("c", 3)]
    }

    #[tokio::test]
    async fn dormant_query_truncates_and_fires_the_load_trigger_once() {
        let engine = SemanticEngine::new();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fired_clone = fired.clone();
        engine.set_load_trigger(Box::new(move || {
            fired_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));

        let out = query(&engine, "anything", lexical(), 2, None).await;
        assert_eq!(out.len(), 2, "lexical fallback respects the limit");
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 1);

        query(&engine, "again", lexical(), 2, None).await;
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "trigger fires exactly once"
        );
        assert_eq!(engine.status(), SemanticStatus::Loading);
    }

    #[tokio::test]
    async fn disabled_query_stays_lexical_without_refiring() {
        let engine = SemanticEngine::new();
        engine.mark_disabled();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fired_clone = fired.clone();
        engine.set_load_trigger(Box::new(move || {
            fired_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let out = query(&engine, "q", lexical(), 1, None).await;
        assert_eq!(out.len(), 1);
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(engine.status(), SemanticStatus::Disabled);
    }

    #[tokio::test]
    async fn ready_query_fuses_with_an_empty_semantic_side() {
        let engine = SemanticEngine::new();
        engine.mark_ready();
        // No embedder installed, so the semantic side returns empty and the
        // fused result is the lexical list, still limited.
        let out = query(&engine, "q", lexical(), 3, None).await;
        assert_eq!(out.len(), 3);
    }
}
