# Changelog

All notable changes to Continuum are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- PHP (`.php`, `.phtml`) and Lua (`.lua`) indexing. PHP contributes functions,
  methods, classes, interfaces, traits and enums, with PHPUnit `test*` methods
  flagged as tests; Lua contributes named function declarations, indexed under
  their final segment so `function M.foo()` matches calls to `M.foo()`.

### Changed

- Bumped tree-sitter to 0.25 and every grammar to its ABI 15 release, which the
  PHP and Lua grammars require.

## [0.1.6] - 2026-06-02

### Fixed

- Check the large-root guard before restoring the warm-start snapshot. The
  guard added in 0.1.5 ran after the restore, so a daemon pointed at a home
  directory or drive root still loaded a previously written (and possibly
  multi-gigabyte) `graph.json` before bailing. An oversized root now skips
  snapshot restore, indexing, and watching alike.

## [0.1.5] - 2026-06-02

### Added

- Guard against out-of-memory when a workspace root is too broad: the daemon
  skips automatic indexing and recursive watching when the root is a filesystem
  root or the user's home directory (override with `CONTINUUM_ALLOW_LARGE_ROOT`),
  and a single index pass is capped at `CONTINUUM_MAX_FILES` files (default
  50000, `0` disables). Memory and on-demand text search keep working.

### Changed

- Broaden the default skipped directories (version-control metadata, dependency
  stores, build output, language caches, and home-directory bloat such as
  `AppData`, `Library`, `.cache`, `.cargo`, and `vendor`).

## [0.1.4] - 2026-05-20

### Fixed

- Quote the release tag/package-version check so the shell cannot evaluate the
  JavaScript template string, and remove an unsupported setup-node input.

## [0.1.3] - 2026-05-20

### Fixed

- Use npm Trusted Publishing correctly by running publish on Node 24, matching
  the configured GitHub environment, and letting npm use OIDC instead of a
  `NODE_AUTH_TOKEN` secret.

## [0.1.2] - 2026-05-20

### Fixed

- Run both macOS release builds on `macos-latest` so npm publishing is not
  blocked waiting for an unavailable `macos-13` runner.

## [0.1.1] - 2026-05-20

### Added

- Multi-agent MCP server with a daemon + thin-adapter architecture over TCP
  loopback, with a token handshake and one daemon per workspace.
- Code knowledge graph built from tree-sitter parsing of Rust, Python,
  JavaScript, TypeScript, and Go, kept current by a debounced filesystem
  watcher. Indexing and `find_text` honour `.gitignore` and skip hidden files.
- 13 MCP tools: `search_code`, `find_text`, `get_file_outline`,
  `get_symbol_definition`, `find_callers`, `get_local_graph`, six cross-agent
  memory tools, and `get_stats` for index diagnostics.
- Hybrid `search_code` — BM25 lexical ranking fused with model2vec semantic
  embeddings via reciprocal rank fusion.
- SQLite-backed cross-agent memory: architectural decisions, an action-history
  log, and an append-only scratchpad.
- Lazy embedding-model load so daemon startup memory stays bounded until the
  first semantic search request.
- Graceful shutdown on Ctrl-C / SIGTERM.
- Reliability limits: AST-depth cap, file-size cap, clamped tool arguments,
  a bounded framed-message size, and a concurrent-connection cap.
- Environment-variable configuration: `CONTINUUM_MODEL`,
  `CONTINUUM_PRELOAD_MODEL`, `CONTINUUM_IDLE_MINUTES`,
  `CONTINUUM_MAX_FILE_KIB`, `CONTINUUM_DEBOUNCE_MS`.
- Distribution: a tag-triggered release workflow that builds prebuilt binaries
  for Linux/macOS/Windows, and the `continuum-mcp` npm wrapper so the server
  runs via `npx`.
- Automated npm publishing from the release workflow with provenance.
- Unit and end-to-end test suites, and a GitHub Actions CI pipeline (fmt,
  clippy, build, test on Linux and Windows).

[Unreleased]: https://github.com/redstone-md/Continuum/compare/v0.1.6...main
[0.1.6]: https://github.com/redstone-md/Continuum/releases/tag/v0.1.6
[0.1.5]: https://github.com/redstone-md/Continuum/releases/tag/v0.1.5
[0.1.4]: https://github.com/redstone-md/Continuum/releases/tag/v0.1.4
[0.1.3]: https://github.com/redstone-md/Continuum/releases/tag/v0.1.3
[0.1.2]: https://github.com/redstone-md/Continuum/releases/tag/v0.1.2
[0.1.1]: https://github.com/redstone-md/Continuum/releases/tag/v0.1.1
