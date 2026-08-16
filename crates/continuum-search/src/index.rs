//! The in-memory vector index and the engine that drives it.
//!
//! Symbol counts are modest (thousands), so the index is a flat list scanned
//! by brute-force cosine similarity per query — sub-millisecond at this scale,
//! and no approximate-nearest-neighbour structure to maintain.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use continuum_core::dto::SearchHit;
use tokio::sync::RwLock;

use crate::embedder::Embedder;

const STATE_DORMANT: u8 = 0;
const STATE_LOADING: u8 = 1;
const STATE_READY: u8 = 2;
const STATE_DISABLED: u8 = 3;

/// Lifecycle of the semantic engine, surfaced by `get_stats` and used to decide
/// whether `search_code` fuses lexical + semantic results or falls back to
/// lexical-only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SemanticStatus {
    /// Created but no model load requested yet.
    Dormant,
    /// A model load is in flight.
    Loading,
    /// Model installed and serving semantic results.
    Ready,
    /// Model load was attempted and failed; lexical-only from here on.
    Disabled,
}

impl SemanticStatus {
    pub fn label(self) -> &'static str {
        match self {
            SemanticStatus::Dormant => "dormant",
            SemanticStatus::Loading => "loading",
            SemanticStatus::Ready => "ready",
            SemanticStatus::Disabled => "disabled",
        }
    }
}

/// A symbol handed to the semantic index for embedding.
pub struct SymbolDoc {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: usize,
    pub signature: String,
    pub is_test: bool,
    /// The text actually embedded (typically `name + signature`).
    pub embed_text: String,
}

struct Entry {
    name: String,
    kind: String,
    path: String,
    line: usize,
    signature: String,
    is_test: bool,
    vector: Vec<f32>,
}

#[derive(Default)]
struct SemanticIndex {
    by_file: HashMap<String, Vec<Entry>>,
}

impl SemanticIndex {
    fn search(&self, query: &[f32], limit: usize, kind: Option<&str>) -> Vec<SearchHit> {
        let mut scored: Vec<(f32, &Entry)> = Vec::new();
        for entries in self.by_file.values() {
            for entry in entries {
                if kind.is_some_and(|k| entry.kind != k) {
                    continue;
                }
                scored.push((dot(query, &entry.vector), entry));
            }
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(score, e)| SearchHit {
                name: e.name.clone(),
                kind: e.kind.clone(),
                path: e.path.clone(),
                line: e.line,
                signature: e.signature.clone(),
                is_test: e.is_test,
                score,
            })
            .collect()
    }
}

/// Embedding-backed semantic search over the workspace's symbols.
///
/// The engine is created immediately at daemon startup but stays **dormant**
/// until [`activate`](Self::activate) installs the embedding model — which a
/// background task does once the model has loaded. While dormant it accepts no
/// documents and answers queries with an empty result, so the daemon never
/// blocks startup on a model download.
pub struct SemanticEngine {
    embedder: OnceLock<Embedder>,
    index: RwLock<SemanticIndex>,
    state: AtomicU8,
    /// Fired once, on the first query while dormant, to start the model load.
    load_trigger: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for SemanticEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticEngine {
    pub fn new() -> Self {
        Self {
            embedder: OnceLock::new(),
            index: RwLock::new(SemanticIndex::default()),
            state: AtomicU8::new(STATE_DORMANT),
            load_trigger: Mutex::new(None),
        }
    }

    /// Install the embedding model. Idempotent — only the first call takes.
    pub fn activate(&self, embedder: Embedder) {
        let _ = self.embedder.set(embedder);
    }

    /// Where the engine is in its load lifecycle. `Ready` is the only state in
    /// which queries fuse semantic results.
    pub fn status(&self) -> SemanticStatus {
        match self.state.load(Ordering::SeqCst) {
            STATE_LOADING => SemanticStatus::Loading,
            STATE_READY => SemanticStatus::Ready,
            STATE_DISABLED => SemanticStatus::Disabled,
            _ => SemanticStatus::Dormant,
        }
    }

    /// Install the callback fired when the engine leaves the dormant state —
    /// the host's "load the model and re-index" task.
    pub fn set_load_trigger(&self, trigger: Box<dyn Fn() + Send + Sync>) {
        *self.load_trigger.lock().unwrap() = Some(trigger);
    }

    /// Move dormant → loading and fire the load trigger, exactly once. No-op in
    /// any other state. If no trigger is installed the transition is rolled
    /// back, so a later `set_load_trigger` + `kick` still works.
    pub fn kick(&self) {
        if !self.begin_loading() {
            return;
        }
        // Take the trigger out before invoking it: the callback must not run
        // under the lock (it may spawn, panic, or re-enter this engine).
        let trigger = self.load_trigger.lock().unwrap().take();
        match trigger {
            Some(trigger) => trigger(),
            None => self.state.store(STATE_DORMANT, Ordering::SeqCst),
        }
    }

    fn begin_loading(&self) -> bool {
        self.state
            .compare_exchange(
                STATE_DORMANT,
                STATE_LOADING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Call after activation and re-indexing complete: queries now fuse.
    pub fn mark_ready(&self) {
        self.state.store(STATE_READY, Ordering::SeqCst);
    }

    /// Call when the model load failed: stay lexical-only permanently.
    pub fn mark_disabled(&self) {
        self.state.store(STATE_DISABLED, Ordering::SeqCst);
    }

    /// Embed and store all symbols of one file, replacing any prior entries.
    /// A no-op while the engine is dormant.
    pub async fn index_file(&self, path: &str, docs: Vec<SymbolDoc>) {
        let Some(embedder) = self.embedder.get() else {
            return;
        };
        if docs.is_empty() {
            self.index.write().await.by_file.remove(path);
            return;
        }
        let texts: Vec<String> = docs.iter().map(|d| d.embed_text.clone()).collect();
        let vectors = embedder.embed(&texts);
        let entries: Vec<Entry> = docs
            .into_iter()
            .zip(vectors)
            .map(|(doc, vector)| Entry {
                name: doc.name,
                kind: doc.kind,
                path: doc.path,
                line: doc.line,
                signature: doc.signature,
                is_test: doc.is_test,
                vector: normalize(vector),
            })
            .collect();
        self.index
            .write()
            .await
            .by_file
            .insert(path.to_string(), entries);
    }

    pub async fn remove_file(&self, path: &str) {
        self.index.write().await.by_file.remove(path);
    }

    /// Embed `query` and return the nearest symbols by cosine similarity.
    /// Returns empty while the engine is dormant.
    pub async fn search(&self, query: &str, limit: usize, kind: Option<&str>) -> Vec<SearchHit> {
        let Some(embedder) = self.embedder.get() else {
            return Vec::new();
        };
        let q = normalize(embedder.embed_one(query));
        self.index.read().await.search(&q, limit, kind)
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kick_without_a_trigger_rolls_back_to_dormant() {
        let engine = SemanticEngine::new();
        engine.kick();
        assert_eq!(engine.status(), SemanticStatus::Dormant);

        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fired_clone = fired.clone();
        engine.set_load_trigger(Box::new(move || {
            fired_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        engine.kick();
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(engine.status(), SemanticStatus::Loading);
    }

    #[test]
    fn normalize_yields_unit_length() {
        let n = normalize(vec![3.0, 4.0]);
        let len = (n[0] * n[0] + n[1] * n[1]).sqrt();
        assert!((len - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_zero_vector_is_safe() {
        assert_eq!(normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn cosine_of_identical_normalized_vectors_is_one() {
        let a = normalize(vec![1.0, 2.0, 3.0, 4.0]);
        assert!((dot(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        let a = normalize(vec![1.0, 0.0]);
        let b = normalize(vec![0.0, 1.0]);
        assert!(dot(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn index_search_ranks_nearest_entry_first() {
        let mut index = SemanticIndex::default();
        index.by_file.insert(
            "f.rs".to_string(),
            vec![
                Entry {
                    name: "near".to_string(),
                    kind: "function".to_string(),
                    path: "f.rs".to_string(),
                    line: 1,
                    signature: String::new(),
                    is_test: false,
                    vector: normalize(vec![1.0, 0.1, 0.0]),
                },
                Entry {
                    name: "far".to_string(),
                    kind: "function".to_string(),
                    path: "f.rs".to_string(),
                    line: 2,
                    signature: String::new(),
                    is_test: false,
                    vector: normalize(vec![0.0, 0.0, 1.0]),
                },
            ],
        );
        let query = normalize(vec![1.0, 0.0, 0.0]);
        let hits = index.search(&query, 10, None);
        assert_eq!(hits[0].name, "near");
    }
}
