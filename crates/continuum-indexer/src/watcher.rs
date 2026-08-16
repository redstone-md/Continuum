//! Filesystem watcher with a ~300 ms debouncer. Coalesced change batches are
//! re-indexed into the graph under a single write lock.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use continuum_core::Settings;
use continuum_graph::CodeGraph;
use continuum_search::SemanticEngine;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{mpsc, RwLock};

use crate::is_skipped_path;

/// Enough room for bursty editor saves without letting build storms retain an
/// unbounded number of paths in memory.
const WATCH_QUEUE_CAP: usize = 4096;

/// Begin watching `root`. The returned `RecommendedWatcher` must be kept alive
/// for watching to continue -- dropping it stops the watch. The debounce window
/// and per-file size cap come from `settings`.
pub fn start_watcher(
    root: PathBuf,
    graph: Arc<RwLock<CodeGraph>>,
    semantic: Arc<SemanticEngine>,
    settings: &Settings,
) -> notify::Result<RecommendedWatcher> {
    let debounce = settings.debounce;
    let max_file_bytes = settings.max_file_bytes;
    let (tx, mut rx) = mpsc::channel::<PathBuf>(WATCH_QUEUE_CAP);
    let callback_root = root.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                if is_skipped_path(&callback_root, &path) {
                    continue;
                }
                if tx.try_send(path).is_err() {
                    tracing::warn!("filesystem event queue is full; dropping path");
                }
            }
        }
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut batch: HashSet<PathBuf> = HashSet::new();
            batch.insert(first);

            let timer = tokio::time::sleep(debounce);
            tokio::pin!(timer);
            loop {
                tokio::select! {
                    _ = &mut timer => break,
                    maybe = rx.recv() => match maybe {
                        Some(path) => { batch.insert(path); }
                        None => break,
                    },
                }
            }

            for path in &batch {
                crate::reindex_one(&root, path, &graph, &semantic, max_file_bytes).await;
            }
            let mut guard = graph.write().await;
            continuum_graph::resolver::resolve(&mut guard);
        }
    });

    Ok(watcher)
}
