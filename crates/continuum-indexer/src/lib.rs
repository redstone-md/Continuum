//! The indexer: filesystem watching plus tree-sitter parsing that keeps the
//! [`continuum_graph::CodeGraph`] — and, when enabled, the semantic search
//! index — in sync with files on disk.

mod languages;
mod parser;
mod textsearch;
mod watcher;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use continuum_graph::CodeGraph;
use continuum_search::{SemanticEngine, SymbolDoc};
use tokio::sync::RwLock;

use languages::Lang;

pub use textsearch::search_text;
pub use watcher::start_watcher;

/// Directory names never descended into during indexing.
///
/// Covers version-control metadata, dependency stores, build output, language
/// caches, and the bulky home-directory trees (`AppData`, `Library`) that a
/// misaimed workspace root would otherwise drag in. These are the safety net
/// over `.gitignore`/`.ignore`, which many of these directories lack.
const SKIP_DIRS: &[&str] = &[
    // Version control
    ".git",
    ".svn",
    ".hg",
    // Continuum's own state
    ".continuum",
    // Dependency stores
    "node_modules",
    "vendor",
    "Pods",
    // Build output
    "target",
    "dist",
    "build",
    "out",
    // Python environments and caches
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    // JS/TS framework output
    ".next",
    ".nuxt",
    "coverage",
    // Language / tool caches
    ".cache",
    ".cargo",
    ".rustup",
    ".npm",
    ".gradle",
    ".m2",
    // Editor / OS home-directory bloat
    ".idea",
    "AppData",
    "Library",
];

/// Full one-shot index of a workspace. Returns the number of files indexed.
///
/// Every file's symbols are fed to `semantic` alongside the graph update; the
/// engine itself decides whether to embed them (it stays dormant until its
/// model has loaded — see [`continuum_search::SemanticEngine`]).
pub async fn index_workspace(
    root: &Path,
    graph: Arc<RwLock<CodeGraph>>,
    semantic: Arc<SemanticEngine>,
) -> usize {
    let mut indexed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for abs in collect_source_files(root) {
        // Parsing is CPU-bound and lock-free; only the graph update takes the lock.
        if let Some((rel, parsed)) = parse_path(root, &abs) {
            let docs = symbol_docs(&parsed);
            {
                let mut guard = graph.write().await;
                guard.replace_file(&rel, parsed.file_node, parsed.symbols);
            }
            semantic.index_file(&rel, docs).await;
            indexed.insert(rel);
        }
    }
    // Evict files that vanished while the daemon was offline — relevant when
    // the graph was warm-started from a snapshot.
    let removed = {
        let mut guard = graph.write().await;
        let removed = guard.retain_files(&indexed);
        continuum_graph::resolver::resolve(&mut guard);
        removed
    };
    for path in removed {
        semantic.remove_file(&path).await;
    }
    indexed.len()
}

/// Re-index a single path after a filesystem change (or drop it if deleted).
pub(crate) async fn reindex_one(
    root: &Path,
    abs: &Path,
    graph: &Arc<RwLock<CodeGraph>>,
    semantic: &Arc<SemanticEngine>,
) {
    if is_skipped_path(root, abs) {
        return;
    }
    let supported = abs
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| Lang::from_extension(e).is_some())
        .unwrap_or(false);
    if !supported {
        return;
    }
    let rel = rel_path(root, abs);
    if abs.is_file() {
        if let Some((_, parsed)) = parse_path(root, abs) {
            let docs = symbol_docs(&parsed);
            {
                let mut guard = graph.write().await;
                guard.replace_file(&rel, parsed.file_node, parsed.symbols);
            }
            semantic.index_file(&rel, docs).await;
            tracing::debug!("re-indexed {rel}");
        }
    } else {
        graph.write().await.remove_file(&rel);
        semantic.remove_file(&rel).await;
        tracing::debug!("dropped {rel}");
    }
}

/// Build embedding documents from a parsed file's symbols.
fn symbol_docs(parsed: &parser::ParsedFile) -> Vec<SymbolDoc> {
    parsed
        .symbols
        .iter()
        .map(|node| SymbolDoc {
            name: node.name.clone(),
            kind: node.kind.as_str().to_string(),
            path: node.path.clone(),
            line: node.start_line,
            signature: node.signature.clone(),
            is_test: node.is_test,
            embed_text: embedding_text(node),
        })
        .collect()
}

/// Text embedded for semantic search: the symbol name plus a snippet of its
/// body, so the model matches on what the code *does*, not only on how it is
/// named. The body is capped so a huge function cannot dilute the vector.
fn embedding_text(node: &continuum_graph::GraphNode) -> String {
    const BODY_BUDGET: usize = 600;
    let body: String = node.source.chars().take(BODY_BUDGET).collect();
    format!("{} {}", node.name, body)
}

/// Largest file indexed, in bytes — override with `CONTINUUM_MAX_FILE_KIB`.
/// Files above this are skipped: generated bundles and vendored blobs, not code
/// an agent navigates by symbol.
static MAX_FILE_BYTES: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    std::env::var("CONTINUUM_MAX_FILE_KIB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|kib| kib * 1024)
        .unwrap_or(2 * 1024 * 1024)
});

fn parse_path(root: &Path, abs: &Path) -> Option<(String, parser::ParsedFile)> {
    let ext = abs.extension()?.to_str()?;
    let lang = Lang::from_extension(ext)?;
    if std::fs::metadata(abs).ok()?.len() > *MAX_FILE_BYTES {
        return None;
    }
    let source = std::fs::read_to_string(abs).ok()?;
    let rel = rel_path(root, abs);
    let parsed = parser::parse(&rel, &source, lang)?;
    Some((rel, parsed))
}

/// Workspace-relative path with forward slashes, used as the graph's file key.
fn rel_path(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Whether a walked entry is a directory the walker never descends into.
///
/// The `ignore` walker already honours `.gitignore`/`.ignore` and hidden
/// files; this is the safety net that catches universal build and dependency
/// directories even in a repository with no ignore rules.
pub(crate) fn is_skipped_dir(entry: &ignore::DirEntry) -> bool {
    entry.file_type().is_some_and(|t| t.is_dir())
        && entry
            .file_name()
            .to_str()
            .map(|n| SKIP_DIRS.contains(&n))
            .unwrap_or(false)
}

pub(crate) fn is_skipped_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .any(|name| SKIP_DIRS.contains(&name))
}

fn collect_source_files(root: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root)
        .require_git(false)
        .filter_entry(|entry| !is_skipped_dir(entry))
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .map(|entry| entry.path().to_path_buf())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_path_matches_nested_build_dirs() {
        let root = Path::new("/workspace");

        assert!(is_skipped_path(
            root,
            Path::new("/workspace/target/debug/a.rs")
        ));
        assert!(is_skipped_path(
            root,
            Path::new("/workspace/pkg/node_modules/a.js")
        ));
        assert!(is_skipped_path(
            root,
            Path::new("/workspace/.venv/lib/site-packages/a.py")
        ));
        assert!(is_skipped_path(
            root,
            Path::new("/workspace/AppData/Local/cache/a.ts")
        ));
        assert!(!is_skipped_path(root, Path::new("/workspace/src/lib.rs")));
    }
}
