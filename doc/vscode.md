# Loopbiotic for VS Code

Status: planned client design. No VS Code code exists yet; this documents the
intended shape of a second protocol client so it reuses the daemon instead of
growing a parallel harness.

## Goal

The VS Code extension should preserve the current Loopbiotic way of working,
not create a separate harness. Session logic, response validation, the patch
gate, goal memory, and prefetch stay in `loopbioticd`. The extension is a
second client of the same protocol, alongside the Neovim integration.

Target flow:

```text
prompt
  → hypothesis
  → finding
  → user decision: follow / why / fix
  → one validated local patch
  → review
  → accept / reject
  → next step
```

Key properties:

- the agent does not modify code before a conscious user decision,
- one card describes one step,
- `follow`, `why`, and `fix` remain separate state transitions,
- one patch covers a small, local, checkable fragment,
- a patch must pass validation before the Apply action is offered,
- accepting one patch does not authorize the remaining changes,
- the next step may be prepared in the background while the current card is
  under review,
- conversation and patch use separate warm model lanes.

## Architecture

```text
                         ┌─ Neovim UI
model ↔ loopbioticd ↔ JSON-RPC
                         └─ VS Code extension
                              ├─ Webview View: cards and streaming
                              ├─ native diff editor: patch review
                              └─ VS Code commands: user actions
```

### `loopbioticd`

The daemon remains the source of truth for:

- the session state machine,
- card contracts,
- context optimization,
- talking to the backend,
- streaming provisional previews,
- patch normalization and validation,
- retry and repair,
- goal memory and completed steps,
- prefetching the next step.

### The VS Code extension

A thin TypeScript layer:

1. Starts `loopbioticd --stdio` as a persistent process.
2. Performs the protocol handshake.
3. Sends the active editor's context over JSON-RPC.
4. Renders cards and progress events.
5. Registers commands corresponding to card actions.
6. Shows a validated patch in the native diff editor.
7. Sends the accept/reject result with the current document state.

The extension must not reimplement transition logic or patch validation.

## Interface

### Webview View

A view in the side panel or Secondary Side Bar renders one current card. It
should handle the states:

```text
Thinking → Streaming draft → Validating → Review → Result
```

A provisional preview may expose only safe descriptive fields:

- `title`,
- `claim`,
- `finding`,
- `question`,
- `explanation`,
- `reason`,
- `summary`.

Apply must not be offered and no change may execute during streaming. The
final card's actions appear only after the complete response is received and
the patch gate has passed.

The webview should preserve editor focus, support the keyboard, and update the
existing card instead of appending further copies of a partial response.

### Patch review

A validated patch is presented through VS Code's native diff editor:

- left side: the current document,
- right side: a virtual document with the proposal,
- Accept: apply a controlled `WorkspaceEdit`,
- Reject: discard without modifying the file,
- Retry: a new attempt for the same step,
- Why: return to the explanation without losing the pending patch.

Before Apply, the extension re-checks the document version. Drift refuses the
apply and returns to retry/repair instead of silently fitting the change to
different content.

## VS Code context

Map into `ContextBundle`:

- the active document's URI and path,
- cursor position,
- selection,
- the visible or bounded buffer fragment,
- unsaved document text,
- diagnostics from `vscode.languages.getDiagnostics`,
- definitions, declarations, implementations, and references via commands/LSP,
- workspace symbols with a short deadline,
- the current document version for drift control.

Collecting more expensive hints should have a hard time budget. The model
request must not wait unboundedly on a symbol provider or language server.

## Latency and telemetry

The extension should record monotonic measurement points:

```text
submit
  → context ready
  → daemon request
  → provider request
  → first delta
  → first provisional preview
  → complete response
  → validated card
  → rendered card
```

Prompt, preview, and patch content stays redacted in logs. Event type, phase,
time, byte counts, hashes, and token metrics may be visible.

## Rollout order

1. Minimal TypeScript extension and `loopbioticd` process lifecycle.
2. Handshake and JSON-RPC request/response/notification handling.
3. Active-editor context capture.
4. Webview with hypothesis, finding, choice, working, and summary cards.
5. Streaming provisional preview without active final actions.
6. Native diff editor for one validated proposal.
7. Accept/reject/retry/why/follow/fix as VS Code commands.
8. Drift control and a safe `WorkspaceEdit`.
9. Prefetch and session restore after the view reopens.
10. Extension ↔ daemon integration tests and TTFT measurements.

## Definition of done

The integration is ready when:

- the same scenario yields equivalent state transitions in Neovim and VS Code,
- the first preview appears without waiting for the final JSON,
- no file-changing action is available before validation,
- one Accept applies only one approved step,
- cancellation stops the active backend turn,
- document drift is detected before Apply,
- restarting the view does not needlessly kill a warm daemon,
- logs allow measuring TTFT without exposing user code.
