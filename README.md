<p align="center">
  <img src="assets/banner.svg" alt="Continuum" width="820">
</p>

<p align="center">
  <a href="https://github.com/redstone-md/Continuum/actions/workflows/ci.yml">
    <img src="https://github.com/redstone-md/Continuum/actions/workflows/ci.yml/badge.svg" alt="CI">
  </a>
  <img src="https://img.shields.io/badge/license-MIT-3b82f6" alt="License: MIT">
  <img src="https://img.shields.io/badge/built_with-Rust-f97316" alt="Built with Rust">
  <img src="https://img.shields.io/badge/MCP-server-8b5cf6" alt="MCP server">
</p>

A persistent, multi-agent **Model Context Protocol (MCP)** server. Continuum gives
AI coding agents — Claude Code, OpenCode, Codex, Gemini CLI — a shared, live view
of a codebase and a memory that survives across agents and sessions.

## Why

Standard MCP servers are spawned per-agent over stdio and forget everything when
the agent exits. Continuum runs a single long-lived daemon per workspace: agents
come and go, but the code graph and the inter-agent memory persist. One agent can
hand off architectural intent to the next without re-deriving context.

## Features

- **Live code graph** — tree-sitter parsing of Rust, Python, JavaScript,
  TypeScript, Go, PHP and Lua, kept in sync by a filesystem watcher.
- **Hybrid code search** — `search_code` fuses lexical BM25 ranking with semantic
  embeddings (a local, pure-Rust model2vec model) via reciprocal rank fusion. One
  compact row per hit — a token-efficient replacement for grep.
- **Code navigation** — file outlines, symbol definitions, caller lookup, and
  local dependency graphs, served from the in-memory code graph.
- **Cross-agent memory** — architectural decisions, an action-history log of
  agent intents, and an append-only scratchpad for handoffs.
- **Daemon + thin adapter** — one stateful daemon per workspace; each agent runs
  a lightweight stdio↔TCP proxy that auto-spawns the daemon on demand.

## Architecture

```
  AI agent ──stdio(MCP)──> continuum-adapter ──TCP loopback──> continuum-daemon
                           (thin proxy)                       (graph + memory)
```

The daemon holds the AST knowledge graph in memory and persists agent memory to a
local SQLite database. Adapters are stateless. See [DESIGN.md](DESIGN.md) for the
full design.

## Installation

### Quick start — `npx`

No build step. Point any MCP-capable agent at:

```json
{
  "command": "npx",
  "args": ["-y", "continuum-mcp"]
}
```

The package downloads the prebuilt binaries for your platform on first use —
Linux x64, macOS x64/arm64, and Windows x64 are supported.

### Prebuilt binaries

Download `continuum-daemon` and `continuum-adapter` for your platform from the
[latest release](https://github.com/redstone-md/Continuum/releases) and keep them in
the same directory.

### From source

With a recent stable Rust toolchain, from a checkout of this repository:

```
sh scripts/install.sh          # Linux / macOS
powershell scripts/install.ps1 # Windows
```

The script builds and installs `continuum-adapter` and `continuum-daemon` into
`~/.cargo/bin` — no registry or account involved. By hand it is just:

```
cargo install --path crates/continuum-adapter
cargo install --path crates/continuum-daemon
```

Or run `cargo build --release` and use the binaries from `target/release/`.

Semantic search lazily downloads a ~30 MB embedding model from HuggingFace on
the first `search_code` call; until then, and if loading fails, search uses
lexical-only ranking and everything else works unchanged.

> **Windows without Visual Studio:** with no MSVC linker, build against the
> llvm-mingw toolchain — `rustup target add x86_64-pc-windows-gnullvm` and add a
> `.cargo/config.toml` selecting that target.

## Connecting an agent

Continuum speaks MCP over stdio. The simplest setup is the `npx` command above;
to use a built binary instead, point the agent at `continuum-adapter`:

```json
{
  "command": "/absolute/path/to/continuum-adapter",
  "args": []
}
```

Either way the adapter auto-spawns the per-workspace daemon and treats the
current working directory as the workspace root (or pass `--workspace <path>`).

Per-agent configuration — Claude Code, Codex CLI, Gemini CLI, and OpenCode — is
in [docs/agent-setup.md](docs/agent-setup.md).

## Self-install (for AI agents)

If you are an AI coding agent with shell access, you can install Continuum and
register it with your own MCP host. Follow the three steps.

### 1 — Install the binaries

With a Rust toolchain — one command, nothing to clone:

```
cargo install --git https://github.com/redstone-md/Continuum continuum-adapter continuum-daemon
```

This places `continuum-adapter` and `continuum-daemon` on your `PATH`
(in `~/.cargo/bin`). Without Rust, skip this step and use `npx` in step 2 — it
fetches prebuilt binaries on first run.

### 2 — Register the MCP server with your host

Run the line for your host. If you skipped step 1, replace the command
`continuum-adapter` with `npx` and arguments `-y continuum-mcp`.

- **Claude Code** — `claude mcp add continuum -- continuum-adapter`
- **Codex CLI** — `codex mcp add continuum --transport stdio --command continuum-adapter`
- **Gemini CLI** — add to `~/.gemini/settings.json`:
  ```json
  { "mcpServers": { "continuum": { "command": "continuum-adapter", "args": [] } } }
  ```
- **OpenCode** — add to `opencode.json`:
  ```json
  { "mcp": { "continuum": { "type": "local", "command": ["continuum-adapter"], "enabled": true } } }
  ```

### 3 — Reload and verify

Restart the MCP host or reload its servers, then call the `get_stats` tool — a
JSON reply means Continuum is live. The first `search_code` call downloads a
~30 MB embedding model; until it finishes, search is lexical-only and everything
else works.

Continuum is a local developer tool: nothing to commit, no secrets, one daemon
per workspace that idles out after 30 minutes.

## Configuration

Continuum reads its settings from the environment, so they propagate through the
agent → adapter → daemon spawn chain. All are optional.

| Variable | Default | Purpose |
|----------|---------|---------|
| `CONTINUUM_MODEL` | `minishlab/potion-base-8M` | Embedding-model repo. Set to `off` to disable semantic search (lexical-only). |
| `CONTINUUM_PRELOAD_MODEL` | unset | Set to `1`, `true`, `yes`, or `on` to load semantic search at daemon startup instead of lazily. |
| `CONTINUUM_IDLE_MINUTES` | `30` | Idle minutes before the daemon exits (`0` = never). |
| `CONTINUUM_MAX_FILE_KIB` | `2048` | Largest file size indexed, in KiB. |
| `CONTINUUM_MAX_FILES` | `50000` | Files indexed per pass (`0` = unlimited). Caps memory on huge trees. |
| `CONTINUUM_ALLOW_LARGE_ROOT` | unset | Set truthy to auto-index even when the workspace root is a drive root or home directory. |
| `CONTINUUM_DEBOUNCE_MS` | `300` | Filesystem-watch debounce window. |

## MCP tools

| Tool | Purpose |
|------|---------|
| `search_code` | Ranked symbol search — hybrid lexical + semantic |
| `find_text` | Literal or regex text search across every file — line-precise grep |
| `get_file_outline` | File structure — definitions with bodies folded |
| `get_symbol_definition` | Full source + docstring of a symbol |
| `find_callers` | Every call site of a symbol |
| `get_local_graph` | Recursive tree of what a symbol calls |
| `store_architectural_decision` | Persist a design decision / ADR |
| `read_project_guidelines` | Read all stored decisions and lore |
| `commit_intent` | Log what an agent did and expects next |
| `get_recent_changes` | Recent agent intents |
| `write_scratchpad` | Append to the shared scratchpad |
| `read_scratchpad` | Read recent scratchpad entries |
| `get_stats` | Index health — graph size, semantic-search state, uptime |

## Project layout

```
crates/
  continuum-core       shared domain types, DTOs, protocol
  continuum-transport  JSON-RPC + MCP wire types, IPC framing, stdio proxy
  continuum-graph      in-memory code knowledge graph
  continuum-indexer    tree-sitter parsing + filesystem watcher
  continuum-memory     SQLite-backed agent memory
  continuum-search     semantic search — embeddings + in-memory vector index
  continuum-daemon     the workspace daemon (binary)
  continuum-adapter    the thin MCP adapter (binary)
```

## License

[MIT](LICENSE).
