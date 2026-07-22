# Loopbiotic UI contract

Status: canonical product contract with current implementation gaps listed
explicitly below.

This document owns what Loopbiotic shows: its two user-facing surfaces, their
views, widgets, composition, responsive behavior, and theme integration.
Behavior belongs in [`interactions.md`](interactions.md). Experience character
belongs in [`feeling.md`](feeling.md).

## UI vocabulary

The following terms are exact and must not be used interchangeably:

- **Window** means a stable, user-recognizable product surface.
- **Frame** means a technical Neovim float, border, backing buffer, picker, or
  other rendering mechanism used to build a Window. Frames do not count as
  additional product Windows.
- **View** means the current content state occupying a Window.
- **Widget** means a typed, optionally interactive content block rendered inside
  AgentWindow.

The UI has exactly two Windows:

1. **PromptWindow** — owned by the user.
2. **AgentWindow** — owned by the agent's current turn and response.

There is no third Window. New features must become a View or Widget inside one
of these two surfaces. A developer may use multiple technical frames to render a
surface only when they behave as one Window in focus, movement, visibility, and
lifecycle.

## PromptWindow

PromptWindow contains:

- the editable prompt;
- one always-visible turn mode: `fix`, `explain`, `investigate`,
  `review`, or `propose`;
- agent/model identity when needed;
- a compact, removable summary of context selected in AgentWindow widgets;
- a compact summary of session-selected Markdown instruction Skills;
- submit, close, Skills, mode, and model controls.

There is no automatic or inferred mode. The visible PromptWindow mode is the
response contract selected by the user. `fix` and `propose` lead to Patch Review;
`explain`, `investigate`, and `review` lead to their non-mutating representations.
Prompt text cannot override the visible mode.

Every PromptWindow, including Reply after Reject or a completed response, owns a
mode. `<C-k>` opens a subordinate picker Frame above PromptWindow — the same
Frame the `<C-l>` model picker and the `<C-g>` Skills picker use. Enter picks
the highlighted mode; Escape keeps the current one. Choosing a mode updates the
same PromptWindow title and preserves typed text, attached Widget context,
AgentWindow, and focus ownership. The picker is a Frame, not a third Window.

`<C-g>` opens a session-scoped multiselect Frame above PromptWindow. It lists
safe Markdown files from the workspace root plus configured autoload entries.
Autoloaded entries are visibly marked and locked; other entries can be toggled
with Space and applied with Enter. Escape restores the previous selection. The
Frame closes with PromptWindow, is height-bounded and scrollable when space is
narrow, and never contacts the backend. Its footer labels are derived from the
actual configured bindings.

Selected instruction Skills are summarized in PromptWindow before submission.
The selection belongs to the session: it remains available in Reply, is
snapshotted with each submitted turn, and clears on Stop or Reset. Markdown
selection is not a capability grant and does not create a Widget or product
Window.

PromptWindow does not contain agent responses, progress, Flow, patch controls,
or other agent widgets. Its configuration pickers — mode, model, Skills, and
attached-context removal — all open the same subordinate picker Frame above
PromptWindow rather than new product Windows.

PromptWindow may be open while AgentWindow remains visible. This is the normal
state after a rejected patch and while composing a follow-up to a completed
response. The two surfaces retain distinct focus and ownership; PromptWindow
does not replace or destroy AgentWindow.

A Netrw directory listing is a valid prompt source: the visible listing is
captured as the context of a file-operation request (moving or renaming files
from the tree). A directory source contributes no LSP hints or Flow graph; the
listing itself is the evidence.

Current visual defaults remain a rounded surface near the source cursor, 96
columns by 10 rows, with horizontal padding 4, vertical padding 2, and z-index
200. These values are canonicalized in `lua/loopbiotic/config.lua`, not copied
into new renderers.

The title has this semantic shape:

```text
Loopbiotic {Prompt|Reply} · {mode} · {agent} / {concrete turn model}
```

The title names the model the next turn will actually run: a patch mode
(`fix`/`propose`) shows the patch-drafting model, a discovery mode
(`explain`/`investigate`/`review`) shows the discovery model. The title is never
a model the turn will not use. If the model is unknown, the label is `model?`;
the literal word `default` is never presented as a model. `<C-l>` picks the
model for the current mode's phase, so both the patch and discovery models are
selectable from PromptWindow without becoming a second Window.

Context selected in a Widget must be visible before submit as a compact summary,
for example `3 files · 3 call sites`. The user must be able to inspect and remove
that context. Exact payloads may stay collapsed, but selected context must never
be attached invisibly.

The instruction summary is separate from Widget-selected context. It names the
selected Markdown files rather than presenting their full content in permanent
chrome; the local Skills picker is the inspection and selection route.

## AgentWindow

AgentWindow is the only Window in which agent activity appears. The same surface
is reused for every agent View:

- preparing and thinking;
- streamed, provisional content;
- a final explanation, finding, question, denial, error, or summary;
- structured code or diagnostic information;
- patch Review with the agent comment and explicit `Accept` / `Reject` gate;
- one or more Widgets;
- a Widget's internal chooser or detail View.

A new response changes AgentWindow's content; it does not open another product
Window. Progress must become the response in the same Window. A Widget detail or
file chooser replaces or nests within AgentWindow rather than floating as a new
surface.

Every user-visible response uses the natural language of the most recently
submitted PromptWindow text. An explicit request for a different output language
wins. Identifiers, paths, commands, and source excerpts remain exact. Internal
continuations and contract repairs retain that language because they do not
introduce newer user intent.

The selected non-mutating mode also controls the response's useful shape. A
review of a broad design question may place two to four prioritized, related
findings or alternatives in one Finding body, with concrete symbols, invariants,
reasons, and trade-offs; it is not reduced to a generic recommendation or one
arbitrary next move. Explain answers the requested causal scope directly, while
Investigate stays focused on one falsifiable hypothesis and the next evidence
that would confirm or reject it. These are Views inside the existing
AgentWindow, not additional cards or product Windows.

In `fix` or `propose`, the next AgentWindow response is Patch Review unless the
agent must deny, report an error, request a location, or ask a genuinely blocking
question. The agent cannot silently downgrade a selected patch mode to a
Finding.

AgentWindow does not show general next-intent commands such as Draft, Follow,
Retry, Cancel, or Goal as response-card actions. It may show local controls such
as expand, collapse, navigate, select, deselect, back, Reply, or Quit. `Reply`
opens PromptWindow; it does not submit a backend request. `Quit` ends the entire
session.

Patch Review is the deliberate exception: AgentWindow shows the agent's comment
and `Accept` / `Reject`, because they are the explicit source-mutation boundary,
not alternative prompts. Accept may continue an already authorized Goal. Reject
does not ask the model to process the rejection. Once rejected, the Review View
becomes a `paused/rejected` View with `Reply` and `Quit`; the rejected proposal is
no longer acceptable.

An unresolved Review does not show Reply and blocks the ordinary PromptWindow
route. Accept or Reject is required before new intent can be composed.

### Visibility

AgentWindow has two presentation modes:

- **visible** — full current agent content;
- **wrapped** — the compact representation currently reached with
  `<leader>ph`, retaining session identity and a restore route without showing
  the full content.

`wrapped` is not a third Window and does not end or pause work. The canonical
wrapped placement is the upper-right corner of the owning tab.

AgentWindow belongs to the tab in which it was opened. On another tab it is not
rendered at all, but its visible/wrapped mode and content are retained. Returning
with `<leader>pr` first activates the owning tab and then restores the visible
AgentWindow. AgentWindow must not follow the user into unrelated tabs.

### Composition

Source code remains the primary editor surface. A visible AgentWindow is compact,
theme-native, and positioned so it does not obscure the source evidence it is
explaining. Long content uses scrolling or deliberate detail Views inside the
same Window; it never solves overflow by spawning another response Window.
The first processing frame uses the same resolved source anchor as subsequent
progress and response renders. It must not appear at a fallback position and
then relocate after session state catches up.

Progress, goal, location, cost, and context metadata are subordinate to the
current answer or Widget. Labels use compact scan columns such as `Goal`, `Now`,
`At`, `Turn`, `Budget`, and `Context`. AgentWindow should not become a dashboard.
Local reasoning, response streaming, bounded workspace reads, and response-chain
recovery update that same processing View. They do not create a log surface or
expose private reasoning text; read progress names the concrete local operation
without rendering file contents before the validated card.

## Diff, creation, and file-operation review

The code diff remains in the ordinary editor surface while AgentWindow retains
the agent comment and review controls. The editor is not a third Loopbiotic
Window.

- Modified files use the current editable inline diff presentation.
- A same-file backend batch is represented as a locally ordered sequence of
  ordinary Patch Review Views. AgentWindow and the editor show exactly one hunk
  at a time, with its position in the sequence. Accept replaces it with the next
  queued hunk; no additional Window or model response appears between them.
- Multi-file patch responses are not a review sequence. They fail the backend
  contract and must be regenerated as separate one-file responses.
- A new file is reviewed as a diff from empty content.
- A new directory has no textual diff, so AgentWindow shows a concise creation
  manifest containing the exact workspace-relative path.
- A proposal that needs parent directories presents the directories and new file
  as one creation set.
- A proposed file move or rename has no textual diff either: AgentWindow shows
  an operation manifest — each `Move from -> to` plus target directories that
  do not exist yet — with the agent comment and the same `Accept` / `Reject`
  gate. Netrw grounds the review in the existing parent of the move targets,
  reusing a window that already shows a directory listing when the prompt came
  from one. Nothing on disk changes until Accept.

Before reviewing a new file or directory, Loopbiotic opens the nearest existing
parent directory with Neovim's built-in Netrw. Netrw provides spatial file-tree
context; it is an editor buffer, not a Loopbiotic Window and not the authority
that applies the proposal. AgentWindow remains visible with `Accept` / `Reject`.

For a new file, both the path and the content diff must remain inspectable before
the same Accept. Netrw may reuse an ordinary editor window or split while the
draft remains reachable; the exact editor layout is an implementation choice and
must not create another Loopbiotic Window or hide either part of the proposal.

Because Netrw cannot display a path that does not exist yet as a real entry, the
pending path remains explicit in AgentWindow. If Netrw is unavailable or cannot
safely open the parent, a transient native confirmation frame may present the
same path. That frame is subordinate to AgentWindow and must not become a new
product Window.

## Widget system

Widgets are structured representations inside AgentWindow. They can explain data
and support local interaction, but they are not miniature agents and cannot
start backend work on their own.

All Widgets share a safe frontend/backend envelope:

```text
WidgetEnvelope
  id          stable within the response
  kind        registered widget kind
  version     schema version for that kind
  title       optional user-facing label
  data        kind-specific, schema-validated payload
  provenance  where the data came from
  intents     allowlisted local interaction capabilities
```

The frontend must reject or downgrade unknown kinds, unsupported versions,
invalid payloads, paths outside allowed scope, and executable/arbitrary commands.
The backend chooses among registered Widget kinds; it never supplies rendering
code, keymaps, Neovim commands, or unrestricted callbacks.

A Widget may expose context candidates through a common reference shape:

```text
WidgetContextRef
  id          stable selection identity
  kind        file | location | call_site | symbol | diagnostic
  file        normalized path
  range       optional exact source range
  label       concise display label
  provenance  LSP | editor | diagnostic | validated agent result
```

Selection changes local `pending prompt context`. It does not send a request.
Opening PromptWindow carries the selected references into its visible context
summary. Only submitting that prompt sends them to the agent.

## Flow as the first Widget

Flow is a `callstack` / call-path Widget, not a Window and not a PromptWindow
pane. It renders editor-resolved LSP data inside AgentWindow:

- files, functions, methods, callers, callees, and exact call sites;
- `◆` root, `↑` caller, `↓` callee, fold/loading markers, and explicit partial,
  timeout, truncated, unavailable, and cycle states;
- agent explanations attached to real resolved nodes or call sites;
- tree, focused path, exact-use, and selectable-context Views;
- local navigation, expand/collapse, selection, deselection, and back.

For educational debugging, the agent may request a Flow Widget and annotate
resolved nodes with explanations of where a real failure propagates. The agent
may reference only node IDs present in the frontend's resolved graph. It cannot
invent call edges or replace missing LSP data with an authoritative-looking tree.

Example interaction: the user selects three files or three concrete call sites
in Flow, opens PromptWindow, sees them attached, and asks the agent to build an
integration test from that path. Selection itself performs no agent action.

## Visual language and theming

Loopbiotic inherits the user's Neovim color scheme and uses semantic default
links instead of fixed runtime brand colors:

| Loopbiotic group | Default theme link |
| --- | --- |
| `LoopbioticNormal` | `NormalFloat` |
| `LoopbioticBorder` | `FloatBorder` |
| `LoopbioticTitle` | `Title` |
| `LoopbioticMuted` | `Comment` |
| `LoopbioticAction` | `Special` |
| `LoopbioticGoal` | `Identifier` |

Errors, warnings, additions, and removals use native diagnostic and diff groups.
Meaning cannot depend on one color. Rounded borders are the current default;
spacing and hierarchy, not decoration, carry the design.

## Protected UI invariants

- There are exactly two product Windows: PromptWindow and AgentWindow.
- Frames, pickers, Widget Views, progress, and restore chrome never become new
  product Windows.
- PromptWindow contains user intent; AgentWindow contains agent activity and
  responses.
- All agent states replace content inside the same AgentWindow.
- PromptWindow and AgentWindow can remain visible together without merging their
  ownership or focus.
- Patch Review retains the diff, agent comment, and explicit `Accept` / `Reject`
  inside the two-Window model.
- Netrw is editor context for creation review, never a third product Window or
  the source of acceptance truth.
- Every Widget renders inside AgentWindow through a registered, versioned schema.
- Widget interaction is local; selected context is visible and removable in
  PromptWindow before submission.
- AgentWindow is tab-affine, never follows the user to another tab, and can be
  restored together with its owning tab.
- `wrapped` retains the same AgentWindow identity and work state.
- Source code remains visually dominant and theme colors remain inherited.
- `fix` and `propose` select Patch Review directly without an intermediate
  Finding View.
- Every PromptWindow visibly owns exactly one mode; mode selection reuses a
  subordinate picker and never creates agent work before submit.
- Instruction Skills remain visible, session-scoped PromptWindow context;
  choosing them is local and cannot create agent work before submit.

## Known implementation gaps

The two singleton surfaces, visible/wrapped ownership, off-tab retention,
AgentWindow-only Flow rendering, scoped response controls, selected-context
footer, and new-file Netrw review now follow the normative model. Remaining
contradictions are explicit:

- The frontend has a registered/versioned Widget envelope, but the Rust agent
  response schema still transports Flow selection through legacy `flow_path`
  fields instead of a general `WidgetEnvelope`.
- Selected Widget references use the shared frontend reference shape but are
  serialized to the existing validated context-hint wire format. The backend
  protocol does not yet expose a dedicated `WidgetContextRef` field.
- New files and missing parent directories have a creation manifest, Netrw
  context, collision revalidation, and transactional cleanup; file moves and
  renames have the same treatment through `file_ops`. A directory-only creation
  proposal is still not representable by the patch protocol, and failure to
  open Netrw currently falls back to the AgentWindow manifest rather than a
  dedicated subordinate confirmation Frame.
- Project Intelligence currently supplies a compact deterministic profile and
  inert Markdown instructions. It does not yet expose task-sliced area Views,
  native framework documentation probes, or versioned knowledge-pack status in
  PromptWindow.

## Self-healing protocol

Normative sections describe the approved product. `Known implementation gaps`
describes every intentional mismatch with shipped code. No other silent mismatch
is allowed.

1. Before editing, classify every new element as PromptWindow content,
   AgentWindow content, a View, a Widget, or a technical Frame.
2. Reject designs that require a third Window; reshape them as a View or Widget.
3. Validate visible, wrapped, off-tab, narrow, loading, empty, invalid-payload,
   partial, and error states when relevant.
4. Update normative text when product intent changes. Update or remove a known
   gap whenever implementation moves toward or away from the contract.
5. Re-read `interactions.md` and `feeling.md`; surface ownership affects focus,
   interruption, trust, and product character.
6. Keep implementation pointers at module/file granularity, not line numbers.

Primary reconciliation sources: `lua/loopbiotic/ui.lua`, `prompt.lua`,
`surfaces.lua`, `scope.lua`, `card.lua`, `diff.lua`, `creation.lua`, `widgets.lua`,
`flow.lua`, `thinking.lua`, `state.lua`, `config.lua`, the protocol card/context
schemas, `lua/loopbiotic/skills.lua`, the Rust context project adapters,
`README.md`, and Lua interaction, Flow, prompt, and navigation tests.
