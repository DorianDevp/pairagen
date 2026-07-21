# Loopbiotic interaction contract

Status: canonical behavior contract with current implementation gaps listed
explicitly below.

This document owns focus, interruption, tab affinity, Window state, Widget
interaction, prompt context, and backend request boundaries. Visible composition
belongs in [`ui.md`](ui.md). Experience character belongs in
[`feeling.md`](feeling.md).

## Core interaction model

Loopbiotic separates user intent from agent activity:

- PromptWindow is where the user decides what the agent should do.
- AgentWindow is where the agent works, answers, and presents Widgets.

The surfaces may both exist when no turn is running. They cannot represent two
simultaneous conversations. Opening PromptWindow during an active turn is an
interrupt operation, not a parallel request.

```text
PromptWindow: compose intent + inspect/remove attached context
       |
       | submit
       v
AgentWindow: processing -> response and/or Widgets
       |
       | local Widget exploration/selection (no backend request)
       v
pending prompt context
       |
       | open PromptWindow, review context, submit
       +-------------------------------------------> next agent turn
```

## Window state

### PromptWindow

PromptWindow is either closed or open. Opening it captures the user's live editor
context plus any explicit pending context selected in Widgets.

When no agent turn is active, opening PromptWindow does not alter AgentWindow's
response. The user can consult a result or Widget while composing the next
request.

When an agent turn is active, requesting PromptWindow must:

1. invalidate the active frontend turn generation so late output cannot replace
   newer state;
2. send cancellation to the real backend turn;
3. prevent a second backend submission from racing the cancelled turn;
4. open PromptWindow as the next place of user control.

The implementation may show PromptWindow immediately after local invalidation,
but submit must remain blocked until the backend/session can safely accept the
next turn. It must never allow two live turns in one session.

Closing PromptWindow without submitting does not resume the interrupted turn.
Interruption is a real cancellation, not a temporary pause. AgentWindow remains
open with the interrupted or preceding decision. Closing PromptWindow returns
focus to AgentWindow, where the user may open Reply again or Quit the session.

### AgentWindow

AgentWindow has one content lifecycle and two presentation modes:

```text
content: processing -> provisional -> response/widget -> interrupted/error
mode:    visible <-> wrapped
tab:     owner tab visible | foreign tab not rendered
```

- `<leader>ph` changes visible AgentWindow to wrapped. It does not cancel work,
  discard content, or end the session.
- `<leader>pr` unwraps AgentWindow. If invoked from another tab, it first switches
  to AgentWindow's owning tab and then restores the full content.
- Leaving the owning tab removes AgentWindow from the foreign tab without
  changing its content or presentation mode.
- Returning to the owner tab may restore the retained mode automatically;
  `<leader>pr` always provides an explicit route back.
- AgentWindow never migrates to the currently active unrelated tab.

Async progress updates may change AgentWindow content, but they do not steal
editor focus. Visibility and execution are independent: wrapped or off-tab work
continues until completion, explicit cancellation, prompt interruption, or Stop.

## Prompt behavior

- The main prompt opens from Normal or Visual mode. A live selection may be
  captured as ordinary editor context.
- PromptWindow and Reply PromptWindow always initialize with one valid mode.
  The main prompt uses its requested/configured mode; Reply starts from the
  active session's latest submitted mode.
- `<C-k>` opens the mode picker in both insert and normal mode, in the same
  subordinate Frame above PromptWindow as the Skills multiselect: Enter applies
  the highlighted mode, Escape keeps the current one, and a picker opened from
  insert mode returns to insert mode. Picking changes only PromptWindow-local
  state, refreshes the visible title, preserves composed text and attachments,
  and performs no backend request.
- `<C-g>` opens the Markdown Skills multiselect in both insert and normal mode.
  Space changes only the pending session selection, Enter applies it, and Escape
  restores the selection present when the picker opened. Config-autoloaded
  entries cannot be deselected. None of these actions contacts the backend.
- The editing surface appears before backend warmup and context discovery finish.
- Empty input does nothing.
- Model selection uses the same picker Frame, preserves typed text, and targets
  the current mode's phase: `fix`/`propose` set the patch-drafting model,
  `explain`/`investigate`/`review` set the discovery model. It performs no
  backend request. Removing attached Widget context (`<C-x>`) opens the same
  Frame over the removable entries.
- A submitted prompt is stashed before PromptWindow closes. Startup failure
  restores that text on the next open.
- Slash-prefixed words remain ordinary prompt content. Prompt text cannot force
  a card kind or override the mode selected visibly through PromptWindow.
- Modes are strict user-selected contracts. `fix` and `propose` require Patch;
  `explain` and `review` require Finding; `investigate` requires Hypothesis.
  Choice, denial, location, and error remain valid safety exits where allowed.
  The backend must never infer a different mode from keywords or phrasing.
- Context selected in Widgets is presented separately from implicit ranked
  context. The user can inspect and remove it before submit.
- Submitting sends one immutable snapshot of prompt text, the mode visible at
  the instant of submit, editor context, selected Widget references, selected
  Markdown Skills, and the deterministic ProjectProfile. Later local changes
  cannot alter an in-flight request.
- Submission establishes that complete user action and its source anchor before
  AgentWindow enters `processing`. The resulting order is `submit -> processing
  View -> session request`; session transport must not independently create a
  premature processing View.

Widget selection does not silently rewrite prompt text. The user explains the
desired operation in their own words; selected references supply evidence and
scope.

### Project Intelligence and instruction Skills

Project Intelligence is backend-owned context, separate from ranked
source-context optimization. Lua supplies only cheap editor facts: the active
Neovim LSP clients, their workspace-relative roots, versions when announced,
and a bounded allowlist of capabilities. On session start, the Rust profiler
reads workspace-owned manifests, lockfiles, and project descriptors. It performs
no model turn, network request, command execution, or MCP call. Schema version 1
classifies the workspace and supplies bounded technology, area, dependency,
project-command, and editor-tool facts with provenance.

The profiler is a registry of independent Rust adapters, not project-specific
logic. Each adapter declares root markers and is activated automatically only
when its evidence exists. Root facts are read concurrently and active adapters
inspect an immutable fact set in parallel; their outputs are merged and sorted
deterministically. The profile lists the adapter IDs that contributed evidence.

The shipped POC includes package-workspace, TypeScript, Angular, React,
Excalidraw, RxJS, Deno, Nx, Cargo/Rust, Axum, SQLx, Tokio, Docker Compose, and
Neovim-LSP adapters. It resolves installed npm versions from `deno.lock`, Nx
areas and `implicitDependencies`, Cargo workspace members, Rust edition and
selected framework dependencies, Compose services, Deno tasks, and bounded
language-server capabilities. No adapter knows the Libregraf project name.

Instruction discovery lists safe `.md` files at the workspace root and any
configured autoload paths. Entries must remain inside the workspace, be regular
Markdown files, and stay below the configured per-file limit. The protocol also
limits count, per-file bytes, and total bytes. Submitted entries carry relative
path, content hash, provenance, autoload state, and inert text content. Selecting
a file grants no tool or command capability.

The selection persists from session start through every Reply. Reply snapshots
the current selection and updates the harness before the turn begins. Stop and
Reset clear the catalog, selection, and ProjectProfile. A different workspace
cannot replace this metadata inside an active session.

The OpenAI-compatible local backend may supplement submitted context through a
Rust-owned evidence loop. It uses provider-stored response chains per session
phase, resolves every previous function call before appending a Reply, and
rebuilds the full bounded context once if the provider has expired the chain.
Discovery turns and explicit Goals may use at most the configured number of
workspace reads; ordinary Patch turns cannot read beyond the already submitted
context. The only host tools are workspace-relative UTF-8 file reads, literal
text search, and one-level directory listing. Reads, results, line counts, file
counts, file sizes, and directory entries are bounded; dependency and build
trees are excluded from search. Canonical-path checks reject absolute paths,
parent traversal, and symlink escapes.

These tools are not Skills and do not use MCP. They cannot execute commands,
contact the network, edit source, or bypass Patch Review. Tool results remain
untrusted project evidence. The backend executes at most one read from each
provider response and resolves any extra parallel calls as rejected, even when
the provider ignores its serial-tool setting. Reasoning is configurable but
private: AgentWindow receives only a concise phase transition, never reasoning
text. Streaming provisional card content and tool activity update AgentWindow
in place. Cancellation closes the active response stream, and its late data
cannot be saved as session state.

Submitting a Reply through PromptWindow carries that Reply Window's selected
mode through `session/reply`. In `fix` or `propose`, Patch is required;
explanation, investigation, and review modes keep their typed response contracts.
A Reply must never silently inherit or infer a backend-only mode that is absent
from its visible PromptWindow.

## Agent response behavior

All backend lifecycle states render through AgentWindow. Starting another state
replaces or updates the Window's current View instead of opening a new product
Window.

AgentWindow may contain:

- plain explanatory content;
- a structured finding, question, denial, error, or summary;
- progress and provisional output;
- a registered Widget;
- explanatory content attached to Widget nodes.

AgentWindow does not expose general next-intent actions such as Draft, Follow,
Why, Goal, Retry, or Cancel. `Reply` is a local route that opens PromptWindow
without contacting the backend. `Quit` ends the entire session. Global lifecycle
commands such as Stop or explicit interrupt may exist, but they are not rendered
as agent-response choices.

Patch Review is the deliberate exception. AgentWindow exposes `Accept` and
`Reject` beside the agent comment because these actions resolve a pending source
mutation. Accept continues the already authorized Goal; it does not introduce a
new goal. Reject performs no model turn and transfers control to PromptWindow.
While Review is unresolved, the ordinary PromptWindow shortcut is out of scope;
the user must Accept or Reject so the pending mutation cannot be abandoned
implicitly.

Local actions remain valid inside AgentWindow when they do not contact the
backend or mutate source: scroll, expand, collapse, filter, navigate, select,
deselect, inspect details, go back, wrap, and restore.

## Widget contract and provenance

Backend and frontend communicate Widgets through registered, versioned schemas.
The frontend owns rendering, bindings, validation, navigation, and all local
state. The backend may choose a Widget kind and provide or reference data allowed
by that schema.

Widget choice should follow, in descending reliability:

1. an explicit user `/mode` or requested representation;
2. available structured editor/LSP/diagnostic data;
3. the task semantic intent;
4. keywords as a weak hint only.

Keywords such as `callstack` must never be the sole protocol contract. A model
may mention an unsupported Widget; the frontend must safely fall back to plain
content rather than inventing UI or accepting arbitrary commands.

Widget payload rules:

- unknown `kind` or `version` is non-interactive fallback content or a clear
  unsupported state;
- every file and source range is normalized and validated;
- LSP-derived graphs accept only frontend-resolved node IDs and edges;
- `intents` are allowlisted local capabilities, never code or command strings;
- invalid items are omitted or marked invalid without breaking AgentWindow;
- duplicate references share a stable selection identity;
- provenance remains attached when selected context enters PromptWindow.

## Widget interaction and pending context

Widget interaction changes representation or local selection only.

```text
Widget navigation          -> local View state
Widget expand/collapse     -> local View state / safe frontend data lookup
Widget select/deselect     -> pending prompt context
Widget open source         -> editor navigation
Widget back/close          -> AgentWindow's preceding View
Prompt submit              -> the only next agent request
```

A Widget cannot directly:

- submit a backend request;
- mutate source files;
- run shell or Neovim commands received from the agent;
- accept a patch;
- start Goal, Draft, Retry, Follow, Why, or Cancel;
- hide selected context from the user.

Pending context belongs to the current session and survives internal Widget View
changes, wrapping, and an off-tab transition. It must have an explicit clearing
rule on deselection, session Stop/Reset, stale file/range validation, and after a
successful prompt submission. A failed submission keeps it with the restored
prompt so the user's constructed request is not lost.

## Flow interaction

Flow is the first concrete Widget. It visualizes frontend-resolved call hierarchy
and exact uses inside AgentWindow.

Local operations include:

- moving through nodes;
- expanding, collapsing, and lazy-loading branches;
- opening a resolved symbol or exact use in the editor;
- switching between tree, focused call path, exact uses, and context selection;
- selecting files, symbols, or concrete call sites for the next prompt;
- attaching agent explanation to resolved nodes without changing graph truth.

Timeouts, provider absence, cycles, truncation, and partial results remain valid
Widget states. An agent-requested path may contain only IDs present in the pinned
frontend graph. Missing IDs are ignored and cannot be rendered as facts.

Example integration-test flow:

1. The user asks for an educational callstack explanation.
2. AgentWindow renders Flow with explanations at real call sites.
3. The user selects three relevant files or call sites.
4. The user opens PromptWindow; the active agent turn is already complete, so no
   cancellation is needed.
5. PromptWindow shows the three selections as attached context.
6. The user asks for an integration test based on that path and submits.
7. AgentWindow reuses the same surface for processing and the response.

## Focus and source navigation

- Async AgentWindow renders do not steal focus.
- Closing a focused Frame returns the cursor to the window it was entered
  from, never to the tab's first split. When a buffer is visible in several
  splits, anchoring, cursor capture, and navigation use the split the user is
  working in, not whichever window happens to be listed first.
- The first processing render and later progress renders share the resolved
  source anchor; asynchronous state completion cannot move AgentWindow from a
  fallback center to the source cursor.
- Opening a Widget source reference is a local editor navigation action, not an
  agent request.
- Navigation should reuse the owning tab's editor windows where possible and
  clamp positions to live buffers.
- Opening source does not migrate AgentWindow to the destination tab. If a
  deliberate navigation opens another tab, AgentWindow retains its original
  owner and becomes off-tab.
- Restoring AgentWindow with `<leader>pr` takes the user back to that owner tab.
- Frames used to build one Window must act as one focus unit; the user should not
  tab through implementation-only borders or backing buffers.

## Cancellation, Stop, and Reset

- Opening PromptWindow during active work is an explicit interrupt and cancels
  the real turn.
- PromptWindow is the normal cancellation route for active work; AgentWindow
  does not display Cancel as response content.
- Wrapping AgentWindow or leaving its tab never cancels work.
- After Reject, PromptWindow and AgentWindow remain open together. PromptWindow
  owns focus until submit or close. Closing it without submitting returns focus
  to AgentWindow; the Goal stays paused.
- Reply from the paused AgentWindow reopens PromptWindow. Quit ends the complete
  session rather than merely closing one Window.
- Stop ends the session locally immediately, clears pending Widget context, and
  informs the backend without a ceremonial receipt.
- Reset additionally closes both product Windows, stops RPC state, clears owner
  tab and Widget state, and restores any safe editor state.
- Late progress or results from cancelled, stopped, reset, or superseded turn
  generations are ignored and logged.

## Source mutation boundary

The current editable diff and explicit review gate remain canonical.

### Modified file

1. AgentWindow presents the agent comment and Review controls.
2. The editor shows the proposed editable diff while retaining the accepted
   source underneath.
3. The user may edit the proposed content before deciding.
4. Accept verifies source freshness, applies the edited proposal, and reports the
   accepted step.
5. When an authorized Goal has remaining work, Accept continues it automatically
   until the next review boundary or Goal completion.
6. Reject restores accepted source, pauses the Goal locally, and does not run the
   model or ask it to interpret the rejection.
7. AgentWindow changes from Review to a `paused/rejected` View containing Reply
   and Quit. The rejected proposal can no longer be accepted.
8. Reject opens PromptWindow while keeping that AgentWindow View visible. The
   user may explain the rejection and submit a revised request, close
   PromptWindow and return to AgentWindow, or Quit the session.

Sending a compact protocol acknowledgement that a patch was rejected is allowed
only as a state transition; it must not invoke the model, consume turn tokens, or
generate a replacement proposal.

### New file or directory

A creation proposal is inert until accepted.

1. Normalize every target relative to the workspace and resolve the nearest
   existing parent without following a symlink outside the workspace.
2. Preflight collisions, permissions, duplicate targets, loaded-buffer drift,
   and any path that appeared after the proposal was prepared.
3. Open the existing parent directory in built-in Netrw so the user sees the real
   filesystem context. Netrw does not apply the change and is not a Loopbiotic
   product Window.
4. AgentWindow shows the exact creation manifest and agent comment. A new file
   also has an editable diff from empty content. Both the Netrw path context and
   file content remain inspectable before one Accept, whether the editor uses a
   split or a reversible navigation route.
5. Accept creates required directories before files and applies the complete
   validated creation set. A failure must not leave an undocumented partial
   result; newly created empty paths are rolled back when safe, and any remainder
   is reported precisely.
6. Reject creates nothing and follows the same paused-Goal PromptWindow flow as a
   modified-file rejection.

If Netrw is unavailable or cannot open the safe parent, use a transient native
confirmation frame showing the same normalized paths. Acceptance semantics and
validation remain identical; the fallback cannot weaken the transaction.

### Invariants for every proposal

- no source mutation from a Widget selection or ordinary prompt submission;
- every mutation has explicit `Accept` / `Reject`;
- proposed and accepted source remain distinguishable;
- stale-source and path validation occur again at Accept time;
- rejection never generates an automatic replacement;
- each accepted incremental boundary must compile/type-check independently;
- Accept may continue only an already authorized Goal.

## Protected interaction invariants

- There is one active backend turn per session.
- Opening PromptWindow during work cancels that work before another submit.
- PromptWindow is the only route to new intent. Accept may continue an already
  authorized Goal to its next review boundary.
- AgentWindow is the only route for agent progress, responses, and Widgets.
- AgentWindow has stable tab ownership and `pr` restores both tab and Window.
- Wrapped and off-tab states preserve work and content without moving the Window.
- Widget interaction is local and cannot mutate code or contact the backend.
- Selected Widget context is visible, removable, provenance-preserving, and sent
  only with an explicitly submitted prompt.
- Late async output can never overwrite a newer prompt or turn.
- Source mutation keeps the editable diff and explicit `Accept` / `Reject` gate.
- Reject performs no model turn, pauses the Goal, and opens PromptWindow without
  closing AgentWindow.
- A rejected proposal cannot be accepted later; AgentWindow exposes only Reply
  and Quit until the user submits new intent or ends the session.
- New paths are workspace-bound, collision-checked, inert until acceptance, and
  shown in Netrw context when possible.
- User-selected `fix` or `propose` transitions directly to Patch Review on both
  the first submitted prompt and a submitted Reply.
- Every PromptWindow has a visible, picker-controlled mode, and both session
  start and session reply transmit that exact submitted mode.
- ProjectProfile is built by marker-activated Rust adapters from bounded local
  evidence; selected instruction Skills are user/config-derived. Their discovery
  and selection never run a model or grant execution.
- Skill selection persists only for the active session and remains visible in
  PromptWindow before each submit.
- OpenAI-compatible local response chains, reasoning, streaming, and bounded
  read tools remain backend-owned; reads are workspace-confined and cannot
  mutate source or originate intent.
- Missing or unsupported modes are rejected at configuration/RPC boundaries;
  they never fall back to an inferred or hidden contract.

## Known implementation gaps

- Widget envelopes and pending references are validated and allowlisted in the
  frontend, but the backend wire schema remains Flow-specific (`flow_path`) and
  selected references are carried as provenance-tagged context hints rather than
  a dedicated `WidgetContextRef` protocol field.
- Directory creation is supported when it is a parent of a proposed new file;
  the backend cannot yet express a directory-only creation manifest. If Netrw
  cannot open, the exact path remains in AgentWindow but there is no separate
  native fallback confirmation Frame yet.
- Accept-time freshness and path checks are implemented, but the frontend does
  not run a project-specific compile/type-check command at every accepted
  boundary. The backend contract enforces independently valid slices, while
  editor diagnostics remain the only ambient compiler evidence.
- Project Intelligence does not yet follow required-document links inside an
  autoloaded `AGENTS.md`, choose task-relevant area slices, run native framework
  compile probes, or load versioned technology knowledge packs. Exact detected
  versions ground the model today but do not by themselves teach an older model
  Angular 22 or TypeScript 6 APIs.

## Self-healing protocol

Normative sections are authoritative product behavior. `Known implementation
gaps` must enumerate every shipped contradiction until it is repaired.

1. Trace changes through Window ownership, active turn generation, backend RPC,
   local Widget state, selected context, and cleanup.
2. Test visible, wrapped, off-tab, prompt-open, active-turn interruption, late
   result, invalid Widget, failed submit, Stop, and Reset routes when relevant.
3. Any new-intent backend path outside PromptWindow is a contract violation.
   `Accept` is allowed only to continue an already authorized Goal.
4. Any new Widget capability must be local, allowlisted, schema-versioned, and
   safe on malformed input.
5. Update normative text for intentional product changes and update/remove known
   gaps as implementation changes. Never leave a mismatch implicit.
6. Re-read `ui.md` and `feeling.md`; ownership and interruption determine visible
   structure, pacing, and trust.

Primary reconciliation sources: `lua/loopbiotic/init.lua`, `prompt.lua`,
`surfaces.lua`, `scope.lua`, `card.lua`, `flow.lua`, `widgets.lua`,
`creation.lua`, `navigation.lua`, `thinking.lua`, `state.lua`, `commands.lua`,
`keymaps.lua`, protocol card/context schemas, the Rust session/turn engine and
context project-adapter registry, and Lua interactivity, Flow, prompt, and
navigation tests.
