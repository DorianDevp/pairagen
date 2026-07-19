# Repository instructions for agents

## Required product contracts

Before changing this repository, read these three files in full:

1. [`doc/ui.md`](doc/ui.md) — visible surfaces, hierarchy, layout, responsive
   behavior, and theming.
2. [`doc/interactions.md`](doc/interactions.md) — state transitions, focus,
   navigation, actions, review safety, and recovery.
3. [`doc/feeling.md`](doc/feeling.md) — product character, voice, pacing, trust,
   and the intended experience.

They are separate views of one product and are mandatory context, including for
backend or protocol work that can surface new state, progress, errors, cards, or
actions in Neovim.

## Non-negotiable UI model

Loopbiotic has exactly two user-facing product Windows:

- **PromptWindow** owns user intent, the visible turn mode, prompt text, and visible attached
  context.
- **AgentWindow** owns all agent progress, responses, and Widgets.

Use the vocabulary from `doc/ui.md`: a technical Neovim float or buffer may be a
Frame; content inside a Window may be a View or Widget. Frames, Views, pickers,
progress states, file choosers, and Widgets do not create additional product
Windows.

Do not introduce a third product Window. AgentWindow does not render general
next-intent actions. Its deliberate exception is patch review: the agent comment
and `Accept` / `Reject` remain the explicit source-mutation gate. `Accept` may
continue an already authorized Goal; `Reject` pauses it and opens PromptWindow
without running the model. A Widget may change local representation or pending
prompt context, but only an explicit PromptWindow submission may introduce new
intent.

There is no automatic intent-routing mode. The mode visible in PromptWindow is
the user-selected response contract: `fix`/`propose` require Patch,
`explain`/`review` require Finding, and `investigate` requires Hypothesis. Never
infer or replace the selected mode from prompt wording. Every patch remains inert
behind `Accept` / `Reject`.

Every PromptWindow, including Reply, must visibly own one valid turn mode.
`keymaps.modes` opens the local picker; submit snapshots and transmits that exact
mode through both `session/start` and `session/reply`. Never add a mode control
to AgentWindow or contact the backend when the picker selection changes.

## Living-contract rule

The three product documents must self-heal with every change.

- At task start, compare the request and current implementation with all three
  contracts. Identify which statements and invariants are affected.
- During implementation, treat explicit user direction as the intended delta and
  preserve unrelated invariants. Do not redesign adjacent surfaces implicitly.
- Before finishing, reconcile all three documents against the resulting behavior,
  even when only one appears directly affected.
- Update affected text in the same patch as code and tests. Rewrite current-state
  sections in place; do not add design changelog entries.
- Normative sections describe the approved product. Current contradictions are
  allowed only when they are explicit under `Known implementation gaps` in the
  relevant contract. Update or remove those gaps as implementation moves.
- Remove obsolete descriptions. Do not present a known gap as shipped canonical
  behavior and do not leave implemented behavior undocumented.
- When runtime code and a descriptive statement disagree, code is evidence of
  current behavior, not automatic authority for product intent. Resolve the
  conflict using the user's request, protected invariants, tests, and surrounding
  design; then repair every stale side.
- A documentation-only task still requires checking cross-document consistency.
  A backend-only task may leave the files unchanged only after verifying that no
  user-visible state, wording, timing, action, or trust boundary changed.

## Change vocabulary

Use these scope words consistently in plans and handoffs:

- **Tune:** preserve the concept and adjust a property.
- **Rebuild:** the named surface or flow may change structure.
- **Unify:** apply an existing project pattern to another location.
- **Explore:** compare alternatives; do not silently make one the contract.
- **Local:** change only the named component or state.
- **Systemic:** change the shared rule and every affected consumer.

For product-facing changes, be able to state:

```text
Scope:
Intent:
Preserve:
UI delta:
Interaction delta:
Feeling delta:
States checked:
Acceptance condition:
```

This is a reasoning checklist, not a required user-facing form.

## Product change definition of done

A product-facing change is complete only when:

- implementation and tests match the normative contract, or every remaining
  mismatch is explicitly and accurately tracked as a known implementation gap;
- loading, success, empty, error, hidden, disabled, and narrow-layout states were
  considered where relevant;
- visible shortcut labels match actual bindings;
- focus, navigation, cancellation, retry, and source-mutation boundaries remain
  explicit;
- every agent state reuses AgentWindow; new intent originates in PromptWindow,
  while `Accept` only continues an already authorized Goal;
- AgentWindow tab ownership, visible/wrapped state, off-tab behavior, and `pr`
  restoration were preserved or deliberately updated;
- Widget payloads are registered, versioned, schema-validated, provenance-aware,
  and limited to allowlisted local intents;
- Widget-selected context is visible and removable before prompt submission;
- patch review keeps the agent comment, diff, and explicit `Accept` / `Reject`;
  rejection performs no model turn and transfers focus through PromptWindow back
  to AgentWindow when the prompt is closed;
- new-file and directory proposals are workspace-bound, collision-checked, and
  reviewed through Netrw when available without treating it as a product Window;
- new runtime copy matches the voice in `doc/feeling.md`;
- documentation describes the resulting current state rather than the edit that
  produced it.

Keep implementation pointers in the contracts at file/module granularity. Avoid
line-number references and duplicated internal details that cannot be maintained
reliably.
