# Changelog

All notable changes to Loopbiotic are documented in this file.

The project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- Backend turns now run under a configurable deadline
  (`LOOPBIOTIC_TURN_TIMEOUT_SECS`, default 600, `0` disables). A wedged agent
  CLI is killed and respawned on the next turn instead of hanging the session
  forever; the Ollama HTTP client uses the same deadline.
- Stopping or switching the backend now fails in-flight requests instead of
  dropping their callbacks, so the thinking spinner can no longer get stuck.
- RPC sends to an already-exited backend are dropped and logged instead of
  throwing inside scheduled callbacks.
- Managed `loopbioticd` downloads now have connect and transfer time limits,
  and only the expected binary is extracted from the release archive.
- Retry exhaustion and goal-batch shape mismatches in the session harness
  degrade to error cards instead of panicking the daemon.
- Push CI runs again: the workflow triggered on a nonexistent `main` branch.

### Changed

- `initialize` validates the client protocol version when one is supplied and
  returns a structured error (`-32001`) on mismatch instead of failing later
  with cryptic errors.
- The project context scan no longer blocks async worker threads.
- Internal restructuring: `engine.rs`, `codex_app.rs`, and the context crate
  are split into focused modules; duplicated backend and Lua helpers are
  shared; Lua state reset is defined next to the state it resets.

### Added

- Lua tooling (`stylua`, `selene`, LuaLS config) enforced in CI, headless Lua
  unit tests for the patch engine and session state, `loopbioticd` JSON-RPC
  integration tests, session state-machine transition tests, and a real
  daemon round-trip smoke test.

## [0.3.2] - 2026-07-14

### Added

- Added the Loopbiotic logo, the “WHERE HUMAN IS IN THE LOOP” slogan, and an
  animated workflow demo to the README.

### Fixed

- Reveal and draft retry no longer share `<leader>pr`; retry now defaults to
  `<leader>pt`, hidden cards cannot trigger actions, and draft action cards stay
  one row clear of the proposal cursor. Actions that the current card no longer
  offers are also ignored, preventing duplicate requests after a session stops.

## [0.3.1] - 2026-07-14

### Fixed

- Miscounted unified-diff hunk headers (`@@ -a,b +c,d @@` where the line counts
  disagree with the body) are now recomputed from the actual lines instead of
  failing the local patch contract. This was the dominant cause of the
  expensive full-batch `contract_retry` re-draft: models frequently get the
  range counts wrong even when the change itself is correct, and each rejection
  re-ran the entire agentic pass, wasting roughly 50–80% of a turn's tokens and
  doubling its wall-clock time.
- Patch hunks whose context or removed lines drift on leading/trailing
  whitespace or indentation are now located with a whitespace-insensitive match
  and canonicalized back to the exact source text, rather than being rejected.
- Both fixes live in the shared `loopbiotic_patch` normalizer/applier, so they
  apply to every backend (Codex, Claude, Ollama, generic, and stdio); the
  whitespace tolerance is mirrored in the editor's Lua patch applier. Added
  lines are never altered, and genuine content mismatches and ambiguous
  relocations are still rejected.

### Changed

- The card window's token line now splits usage into input (with the cached
  portion in parentheses) and output, and shows an estimated cost per turn and
  per session using per-model pricing.

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
