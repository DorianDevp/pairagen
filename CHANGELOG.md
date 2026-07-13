# Changelog

All notable changes to Loopbiotic are documented in this file.

The project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.3.0] - 2026-07-13

### Changed

- Renamed Pairagen to Loopbiotic across the Lua module, commands, help tags,
  state and data directories, daemon, Rust crates, environment variables,
  streaming records, release artifacts, and repository metadata.
- Existing model preferences are read from the previous state directory when
  the new Loopbiotic preference file does not exist yet.
- The editor/backend protocol is now version 8. Version 0.3.0 is a breaking
  rename with no aliases for the previous public API.

### Fixed

- Action cards now follow the active tab, navigation no longer jumps through a
  matching buffer in an unrelated tab, and patch drafts focus the first added
  character automatically.

## [0.2.0] - 2026-07-13

### Added

- Continuous goals with visible progress, collapsible details, and a `Why`
  side conversation that returns to the pending draft.
- Complete multi-file goal batches, including new files, validated against live
  editor buffers and reviewed locally one hunk at a time.
- Persistent Claude CLI and Ollama sessions, runtime agent/model routing,
  streaming progress, structured denial cards, and `/{kind}` prompt overrides.
- Raw, cached, and non-cached token accounting with a configurable session
  budget guard.
- Automatic local error-diagnostic checks after goal completion and a manual
  zero-token `Check` action.

### Changed

- Goal execution is batch-first: accepting queued hunks and navigating between
  files no longer creates agent turns.
- `open_location` is now a fallback for files the agent could not inspect rather
  than the normal multi-file workflow.
- Speculative patch prefetch is disabled by default.
- Codex discovery and patch work use separate lanes with compact context reuse.
- The editor/backend protocol is now version 7.

### Fixed

- Repeated goal steps after accepting a patch.
- Navigation aliases and deferred navigation for unavailable or newly created
  files.
- Parsing of agent-provided open-location reasons.

## [0.1.0] - 2026-07-12

### Added

- Interactive Neovim pair-programming cards and editable inline patch drafts.
- Codex app-server, generic CLI, stdio agent, and mock backends.
- Deterministic token-budgeted context ranking with LSP and diagnostics support.
- Patch normalization, validation, repair retries, and per-attempt telemetry.
- Managed `loopbioticd` installation from verified GitHub release artifacts.
- Redacted local session logs, log retention, and `:checkhealth loopbiotic` diagnostics.
