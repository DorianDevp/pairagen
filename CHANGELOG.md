# Changelog

All notable changes to Loopbiotic are documented in this file.

The project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- The Codex model picker now reads the authenticated app-server `model/list`
  catalog and its default model. A discovery-only model is no longer presented
  as the entire set of selectable patch models.
- Navigating to a card or draft no longer throws "Cursor position outside
  buffer" when the target line does not exist yet — for example a patch hunk
  that appends to the end of a short file, or an agent-supplied location past
  the end of the buffer. Cursor targets are now clamped to the real buffer.
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
- Goal hunks that no longer apply after the buffer changes now offer a
  one-keypress retry that regenerates only the stale local step.
- Rejecting a draft now stops locally with explicit Retry/Edit/Stop actions
  instead of immediately spending another model turn on a replacement.
- Resuming an already-visible action card now moves focus into its window,
  and patch action cards register every configured shortcut they display
  (accept, reject, retry, why, and go-to) in addition to their single-key aliases.
- Working cards tolerate protocol `null` metadata instead of failing while
  rendering their token footer. Sending a message while a draft is open now
  restores the source and abandons the preview before conversation starts;
  tab navigation never starts from a float, and background-tab action floats
  are closed only after their tab becomes current, avoiding a Neovim tabline
  use-after-free.
- Mechanical model diff wrappers are normalized before validation: CRLF line
  endings, markdown fences, matching git headers, and unambiguous `./`, `a/`,
  or `b/` path prefixes. Prose, rename/copy metadata, unmatched fences, and
  headers naming another file are rejected instead of being silently dropped.
- Cursor-local editor errors remain explicit model context even when their
  source line is already in the primary excerpt. Codex receives the diagnostic
  text on discovery and patch turns, so a distant warning or deprecation no
  longer displaces the error beside the cursor.
- Push CI runs again: the workflow triggered on a nonexistent `main` branch.

### Changed

- Every Prompt and Reply carries a visible user-selected mode. Automatic intent
  routing was removed: fix/propose require Patch, explain/review require Finding,
  and investigate requires Hypothesis. Slash-prefixed text no longer overrides
  that visible mode. Persistent multi-step goal execution remains explicit, and
  every patch stays behind local Accept/Reject review.
- Goal work is limited to one file, one coherent hunk, and 32 changed lines
  per turn. Only explicit goals may speculate on the next patch; ordinary
  speculation is read-only post-accept conversation.
- `initialize` validates the client protocol version when one is supplied and
  returns a structured error (`-32001`) on mismatch instead of failing later
  with cryptic errors.
- The project context scan no longer blocks async worker threads.
- Backend prompts put static contracts and append-only session history before
  volatile action and editor context data, preserving the longest reusable
  provider-cache prefix across turns and sliced goal continuations.
- Internal restructuring: `engine.rs`, `codex_app.rs`, and the context crate
  are split into focused modules; duplicated backend and Lua helpers are
  shared; Lua state reset is defined next to the state it resets.

### Added

- PromptWindow now has a session-scoped Markdown Skills multiselect. Configured
  files such as `AGENTS.md` autoload as locked inert instructions; optional root
  Markdown files are explicitly selected, remain visible through Reply, and are
  content-addressed and bounded before reaching the backend. A deterministic
  ProjectProfile is built in Rust by marker-activated technology adapters and
  supplies exact lockfile versions, Nx/Cargo workspace areas and dependencies,
  commands, Compose infrastructure, and active Neovim LSP capabilities without
  MCP or a model discovery turn. Protocol version is now 12.
- Prompts resolve a session-pinned static Flow graph through LSP call hierarchy
  without opening UI or changing geometry. The explorer is an explicit `F`
  toggle with lazy caller/callee expansion, exact call-site/reference
  navigation and responsive split/single-pane layouts. Callstack answers carry
  a structured `flow_path` and render it directly in the card UI. The bounded
  normalized graph is sent to every backend without agent-side rediscovery.
- Conversation turns have a 10-second visible-response budget and work turns
  a 20-second budget. Slow turns yield a focusable `Working` card, continue in
  the background, and can be interrupted through the real Codex
  `turn/interrupt` API or by terminating persistent CLI processes. Completion
  arrives through `agent/turn_ready`; slow-turn timing is logged locally and
  injected once as compact feedback on the next model turn.
- Accepting a non-goal patch automatically surfaces a read-only conversational
  next card, with no intermediate “local step applied” summary. That card is
  prefetched during review by default; rejecting remains a local decision and
  never regenerates code.
- Backend preflight in the prompt window (failures surface before typing;
  composed prompts survive failed starts), repeated-error escalation with
  actionable guidance, and a client-side error boundary that preserves the
  session when a UI callback fails.
- The `backend/warmup` handshake now reports an explicit identity: the active
  backend, the concrete model the next turn will use (configured, or resolved
  from the backend — the Claude CLI announces it at process start, Ollama
  always knows it), and the models the backend can enumerate (Ollama's local
  tags).
- The prompt window title names the active agent and resolved model (never
  "default"), refreshing as soon as warmup resolves it, and `Ctrl-l`
  (`keymaps.models`) opens a model picker fed by the backend-enumerated
  models, an optional per-agent `models` list, and the last reported model.
  Selections persist per agent exactly like `:LoopbioticModel`.
- Identity is phase-aware: the reported model is always the patch-drafting
  one, and a differing discovery model is shown separately instead of
  masquerading as the active model. The shipped Codex agent uses
  `gpt-5.4-mini` at low effort for conversation; Claude uses
  `discovery_model = "haiku"`. The claude picker offers the CLI aliases
  `sonnet`, `opus`, and `haiku` since the CLI has no model-listing API.
- Lua tooling (`stylua`, `selene`, LuaLS config) enforced in CI, headless Lua
  unit tests for the patch engine and session state, `loopbioticd` JSON-RPC
  integration tests, session state-machine transition tests, and a real
  daemon round-trip smoke test.
- Agent-attempt telemetry includes a closed `violation_class` for contract
  retries and rejected cards, allowing context mismatches, malformed diffs,
  wrong files, missing fields, kind mismatches, duplicate steps, and
  incoherent goal batches to be aggregated without logging patch content.

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
