//! Continuum daemon: one long-lived, stateful process per workspace. Holds the
//! code graph in memory, owns the SQLite-backed agent memory, and serves MCP
//! over a TCP loopback socket to any number of thin adapters.

mod lifecycle;
mod mcp;
mod tools;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use continuum_core::{Handshake, HandshakeReply, LockFile, PROTOCOL_VERSION};
use continuum_graph::CodeGraph;
use continuum_memory::Memory;
use continuum_transport::framing::{read_line, write_line};
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};
use tracing_subscriber::EnvFilter;

use crate::lifecycle::Workspace;

#[derive(Parser)]
#[command(
    name = "continuum-daemon",
    version,
    about = "Continuum workspace daemon"
)]
struct Args {
    /// Workspace root directory.
    #[arg(long)]
    workspace: PathBuf,
    /// Idle minutes before the daemon shuts itself down (0 = never).
    #[arg(long, env = "CONTINUUM_IDLE_MINUTES", default_value_t = 30)]
    idle_minutes: u64,
}

/// Shared daemon state, handed to every connection task.
pub(crate) struct Daemon {
    pub(crate) token: String,
    pub(crate) graph: Arc<RwLock<CodeGraph>>,
    pub(crate) memory: Memory,
    /// Semantic search engine. Dormant until its model loads in the background.
    pub(crate) semantic: Arc<continuum_search::SemanticEngine>,
    pub(crate) semantic_state: Arc<AtomicU8>,
    pub(crate) conns: AtomicUsize,
    pub(crate) last_activity: Mutex<Instant>,
    /// When the daemon started — surfaced as uptime by the `get_stats` tool.
    pub(crate) started_at: Instant,
    /// Workspace root, scanned by the `find_text` tool.
    pub(crate) workspace_root: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let ws = Workspace::resolve(&args.workspace)?;

    // One daemon per workspace: hold the singleton lock for our whole lifetime.
    let _singleton = match ws.acquire_singleton()? {
        Some(lock) => lock,
        None => {
            tracing::info!("another daemon already owns this workspace; exiting");
            return Ok(());
        }
    };

    let memory = Memory::open(&ws.db_path()).map_err(|e| anyhow::anyhow!("open memory: {e}"))?;
    let graph = Arc::new(RwLock::new(CodeGraph::new()));

    // Warm start: restore the previous graph snapshot if one is present. The
    // background re-index below then refreshes it and prunes stale files.
    if let Some(snapshot) = ws.read_snapshot() {
        let files = snapshot.file_count();
        graph.write().await.restore(snapshot);
        tracing::info!("restored {files} files from snapshot");
    }

    // The semantic engine exists immediately but stays dormant until the
    // embedding model finishes loading in the background, so the daemon never
    // blocks startup on a model download.
    let semantic = Arc::new(continuum_search::SemanticEngine::new());

    // A workspace rooted at a drive/filesystem root or the user's home directory
    // would walk an enormous tree and exhaust memory. Refuse to auto-index (and
    // to recursively watch) such a root; the daemon still serves memory and
    // on-demand text search. CONTINUUM_ALLOW_LARGE_ROOT=1 overrides.
    let _watcher = if let Some(reason) = unsafe_index_root(&ws.root_path()) {
        tracing::warn!(
            "skipping automatic indexing: {reason}. Open a project subdirectory, \
             or set CONTINUUM_ALLOW_LARGE_ROOT=1 to override."
        );
        None
    } else {
        // Index in the background so the daemon serves immediately; navigation
        // tools return progressively richer results as the scan completes.
        {
            let graph = graph.clone();
            let semantic = semantic.clone();
            let root = ws.root_path();
            let ws_snapshot = ws.clone();
            tokio::spawn(async move {
                let n = continuum_indexer::index_workspace(&root, graph.clone(), semantic).await;
                tracing::info!("initial index complete: {n} files");
                ws_snapshot.write_snapshot(&graph.read().await.snapshot());
            });
        }
        Some(
            continuum_indexer::start_watcher(ws.root_path(), graph.clone(), semantic.clone())
                .map_err(|e| anyhow::anyhow!("start file watcher: {e}"))?,
        )
    };

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind IPC socket")?;
    let addr = listener.local_addr()?;
    let token = generate_token();

    ws.write_lockfile(&LockFile {
        pid: std::process::id(),
        endpoint: addr.to_string(),
        token: token.clone(),
        protocol_version: PROTOCOL_VERSION,
    })?;
    tracing::info!(
        "continuum daemon listening on {addr} (workspace {})",
        ws.display()
    );

    let daemon = Arc::new(Daemon {
        token,
        graph,
        memory,
        semantic,
        semantic_state: Arc::new(AtomicU8::new(SEMANTIC_DORMANT)),
        conns: AtomicUsize::new(0),
        last_activity: Mutex::new(Instant::now()),
        started_at: Instant::now(),
        workspace_root: ws.root_path(),
    });

    // Keep startup memory bounded. Semantic search loads lazily on the first
    // `search_code` call unless explicitly preloaded for deployments that want
    // full hybrid search ready immediately.
    if semantic_preload_enabled() {
        maybe_start_semantic_load(&daemon);
    }

    if args.idle_minutes > 0 {
        spawn_idle_watch(
            daemon.clone(),
            ws.clone(),
            Duration::from_secs(args.idle_minutes * 60),
        );
    }

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted.context("accept")?;
                // Reserve a connection slot up front; reject past the cap so a
                // flood of connections cannot spawn unbounded tasks.
                if daemon.conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    daemon.conns.fetch_sub(1, Ordering::SeqCst);
                    tracing::warn!("connection cap ({MAX_CONNECTIONS}) reached; rejecting {peer}");
                    continue;
                }
                let d = daemon.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &d).await {
                        tracing::debug!("connection {peer} ended: {e}");
                    }
                    d.conns.fetch_sub(1, Ordering::SeqCst);
                    *d.last_activity.lock().await = Instant::now();
                });
            }
            _ = shutdown_signal() => {
                tracing::info!("shutdown signal received; stopping");
                break;
            }
        }
    }

    // Clean exit: persist a fresh snapshot for the next warm start, then drop
    // the lockfile so the next adapter spawns a fresh daemon instead of dialing
    // a dead endpoint. The singleton lock and file watcher release on drop.
    ws.write_snapshot(&daemon.graph.read().await.snapshot());
    ws.remove_lockfile();
    tracing::info!("continuum daemon stopped");
    Ok(())
}

/// Resolves when the process is asked to shut down: Ctrl-C on any platform,
/// or additionally SIGTERM on Unix.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Hard cap on concurrent adapter connections. Past this the daemon rejects new
/// connections rather than spawning unbounded tasks.
const MAX_CONNECTIONS: usize = 128;

/// A peer must complete the handshake within this window or be dropped, so a
/// connection that never sends anything cannot hold a slot indefinitely.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const SEMANTIC_DORMANT: u8 = 0;
pub(crate) const SEMANTIC_LOADING: u8 = 1;
pub(crate) const SEMANTIC_READY: u8 = 2;
pub(crate) const SEMANTIC_DISABLED: u8 = 3;

fn generate_token() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// Background task: exit once no adapters have been connected for `idle`.
fn spawn_idle_watch(daemon: Arc<Daemon>, ws: Workspace, idle: Duration) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if daemon.conns.load(Ordering::SeqCst) != 0 {
                continue;
            }
            let idle_for = daemon.last_activity.lock().await.elapsed();
            if idle_for >= idle {
                tracing::info!("idle for {idle_for:?}; shutting down");
                ws.remove_lockfile();
                std::process::exit(0);
            }
        }
    });
}

/// Load the embedding model off the startup path. On success, activate the
/// semantic engine and re-index so it embeds every symbol through the same path
/// the indexer uses; on failure the daemon stays on lexical-only search.
fn spawn_model_load(
    semantic: Arc<continuum_search::SemanticEngine>,
    graph: Arc<RwLock<CodeGraph>>,
    root: PathBuf,
    state: Arc<AtomicU8>,
) {
    tokio::spawn(async move {
        match tokio::task::spawn_blocking(continuum_search::Embedder::load).await {
            Ok(Ok(embedder)) => {
                semantic.activate(embedder);
                let n = continuum_indexer::index_workspace(&root, graph, semantic).await;
                state.store(SEMANTIC_READY, Ordering::SeqCst);
                tracing::info!("embedding model ready; semantic search enabled ({n} files)");
            }
            Ok(Err(e)) => {
                state.store(SEMANTIC_DISABLED, Ordering::SeqCst);
                tracing::warn!("semantic search disabled (model load failed): {e}");
            }
            Err(e) => {
                state.store(SEMANTIC_DISABLED, Ordering::SeqCst);
                tracing::warn!("semantic search disabled (load task panicked): {e}");
            }
        }
    });
}

pub(crate) fn maybe_start_semantic_load(daemon: &Arc<Daemon>) {
    if daemon
        .semantic_state
        .compare_exchange(
            SEMANTIC_DORMANT,
            SEMANTIC_LOADING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }

    spawn_model_load(
        daemon.semantic.clone(),
        daemon.graph.clone(),
        daemon.workspace_root.clone(),
        daemon.semantic_state.clone(),
    );
}

/// A human-readable reason if `root` is too broad to auto-index — a filesystem
/// root or the user's home directory — or `None` when it is safe (or the
/// `CONTINUUM_ALLOW_LARGE_ROOT` escape hatch is set). `root` is already
/// canonicalized by [`Workspace::resolve`], as is the home directory here, so
/// the comparison is exact.
fn unsafe_index_root(root: &Path) -> Option<String> {
    if env_flag("CONTINUUM_ALLOW_LARGE_ROOT") {
        return None;
    }
    if root.parent().is_none() {
        return Some(format!("{} is a filesystem root", root.display()));
    }
    if home_dir().is_some_and(|home| home == root) {
        return Some(format!("{} is your home directory", root.display()));
    }
    None
}

/// The user's home directory, canonicalized to match a resolved workspace root.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .and_then(|p| p.canonicalize().ok())
}

/// Whether an environment variable is set to a truthy value.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
}

fn semantic_preload_enabled() -> bool {
    env_flag("CONTINUUM_PRELOAD_MODEL")
}

/// Validate the Continuum handshake, then serve MCP for the connection's life.
/// Connection accounting (the `conns` counter, `last_activity`) is handled by
/// the accept loop's spawn wrapper.
async fn handle_connection(stream: TcpStream, daemon: &Arc<Daemon>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // A peer that connects but never sends a handshake must not hold its slot.
    let line = match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_line(&mut reader)).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => return Ok(()),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            tracing::debug!("handshake timed out");
            return Ok(());
        }
    };
    let hs: Handshake = serde_json::from_str(line.trim()).context("parse handshake")?;
    let ok = hs.protocol_version == PROTOCOL_VERSION && hs.token == daemon.token;
    let reply = HandshakeReply {
        ok,
        error: (!ok).then(|| "protocol version or token mismatch".to_string()),
        protocol_version: PROTOCOL_VERSION,
    };
    write_line(&mut write_half, &serde_json::to_string(&reply)?).await?;
    if !ok {
        tracing::warn!("rejected adapter handshake");
        return Ok(());
    }

    serve_mcp(&mut reader, &mut write_half, daemon).await
}

/// The per-connection MCP request/response loop.
async fn serve_mcp<R, W>(reader: &mut R, writer: &mut W, daemon: &Arc<Daemon>) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    while let Some(line) = read_line(reader).await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        *daemon.last_activity.lock().await = Instant::now();
        if let Some(response) = mcp::handle_message(trimmed, daemon).await {
            write_line(writer, &serde_json::to_string(&response)?).await?;
        }
    }
    Ok(())
}
