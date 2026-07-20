# Loopbiotic feeling contract

Status: canonical product-character contract. Concrete implementation deviations
are tracked in [`ui.md`](ui.md) and [`interactions.md`](interactions.md).

This document owns how Window ownership, Widgets, copy, pacing, interruption,
and control should feel. Visible structure belongs in `ui.md`; concrete behavior
belongs in `interactions.md`.

## Core promise

**Where human is in the loop.**

Loopbiotic should feel like a calm, technically sharp pair programmer working
inside the editor. The user owns intent through PromptWindow. The agent owns one
stable AgentWindow for work, evidence, explanations, and Widgets. Neither side
silently takes over the other's surface.

It is intentionally not:

- a chat transcript competing with code;
- autocomplete that mutates first and explains later;
- a collection of popups for every backend event;
- an autonomous progress dashboard;
- a theatrical AI persona;
- a Widget whose controls quietly become agent commands.

## Experience coordinates

| Axis | Canonical position | Product expression |
| --- | --- | --- |
| Calm ↔ expressive | Strongly calm | Two stable surfaces, native theme, restrained borders, no popup cascade |
| Technical ↔ editorial | Strongly technical | Exact files, ranges, symbols, calls, diagnostics, provenance, and explicit partial states |
| Compact ↔ spacious | Compact with breathing room | Wrapped AgentWindow, short labels, progressive disclosure inside the same surface |
| Immediate ↔ cinematic | Immediate | Prompt opens promptly, interruption restores control, Stop avoids ceremony |
| Guided ↔ autonomous | Strongly guided | The agent represents; the user selects context and submits intent |
| Utilitarian ↔ ornamental | Utilitarian but finished | Editor-native frames, careful geometry, no decorative panel ecosystem |
| Assertive ↔ deferential | Confident, not pushy | Useful structured answer; only the explicit Review gate may continue an authorized Goal |
| Dense ↔ verbose | Information-dense, not noisy | Widgets organize evidence without turning the editor into a dashboard |

Shorthand for critique:

```text
calm · technical · compact · immediate · user-directed · editor-native · inspectable
```

Use directional feedback, for example: “make the wrapped state quieter without
making the active turn harder to recover.” “Make it nicer” is not actionable.

## Surface ownership

The two Windows create a psychological boundary:

- **PromptWindow feels authored by the user.** It contains the user's language,
  selected evidence, and final decision to submit.
- **AgentWindow feels authored by the agent but controlled by the user.** It
  contains work, explanations, and safe representations, never an invitation to
  surrender the next decision.

Reusing AgentWindow across thinking, response, and Widget Views creates
continuity. Spawning a new Window for progress, callstack, files, recovery, or a
new answer feels fragmented and violates the product character even if each
individual popup looks polished.

Frames are implementation detail. The experience must still feel like two
stable surfaces with predictable ownership, focus, visibility, and return paths.

## Emotional rhythm

```text
orient -> compose -> hand off -> observe -> inspect -> select evidence -> compose
```

### Orient

The current buffer, cursor, selection, diagnostics, resolved call graph, and
deterministic project profile make the agent feel situated. Exact versions and
workspace areas point to real project facts rather than generic ecosystem prose.

### Compose

PromptWindow appears as a small editor instrument. The user can see and remove
Widget-selected context before submit, so the request feels intentional rather
than secretly augmented.

The same authorship applies to instruction Skills. Config-autoloaded rules are
plainly marked, optional root Markdown files are chosen in a compact local
multiselect above PromptWindow, and the selected filenames remain visible. The
model never silently chooses its own instructions.

### Hand off

Submission moves activity into AgentWindow. There is one active turn and one
place to watch it. Opening PromptWindow again during work clearly means “stop
this turn; I want to redirect,” not “start another conversation beside it.”
The handoff is causal and visually settled: the complete prompt action exists
first, then AgentWindow reacts in its final anchored position, then the session
request runs. A corrective jump after processing appears feels broken and is not
an acceptable form of responsiveness.

### Observe

Progress reassures without performing intelligence. Elapsed time and a few
concrete phases are enough. Wrapping or changing tabs yields visual space without
pretending work has stopped.

### Inspect

Plain content is enough for plain answers. Widgets appear when structure improves
understanding: a call path, diagnostic set, source locations, or selectable
evidence. Partial knowledge is named honestly.

### Select evidence

Widget selection feels like building context, not issuing commands. The user can
explore freely because selecting a file or call site cannot spend tokens, mutate
code, or trigger the agent.

### Compose again

The selected evidence becomes a visible attachment in PromptWindow. The user
states what to do with it, submits, and AgentWindow continues in the same place.

### Review

A code proposal remains a concrete editable diff, not an abstract Widget action.
AgentWindow explains the change and keeps `Accept` / `Reject` visible as the
mutation boundary. Accept feels like continuing the Goal the user already
authorized. Reject feels respected immediately: no model interpretation, no
replacement attempt, and no token-spending rejection turn.
Until that decision is made, the ordinary prompt route stays quiet; a pending
mutation is never abandoned through a side door.

After Reject, PromptWindow opens without removing AgentWindow. The user can
explain the objection, close PromptWindow and return focus to the paused
AgentWindow, or Quit the whole session. The UI should feel paused and recoverable,
not half-finished or secretly progressing. The rejected proposal must stop
looking actionable; AgentWindow offers Reply and Quit rather than leaving Accept
behind.

For a new path, opening its existing parent in Netrw provides real filesystem
orientation. AgentWindow remains the source of the proposal and approval. A
creation manifest must make nonexistent paths explicit because Netrw cannot show
them as real entries before acceptance.

## Wrapped and tab-affine feeling

Wrapped AgentWindow should feel parked, not destroyed. It communicates:

- the session and content still exist;
- ongoing work still runs;
- the editor has been returned to the user;
- one shortcut restores the full surface.

Moving to another tab should feel like leaving that working context behind, not
dragging the agent across the entire editor. Invoking `pr` from elsewhere should
feel like returning to the conversation: Loopbiotic moves to the owning tab and
restores AgentWindow there.

The wrapped representation stays compact and quiet. It is not a notification
center, progress dashboard, or third surface.

## Widget character

Widgets are explanations with structure. They should feel:

- native rather than embedded-web-like;
- deterministic rather than generative in their controls;
- grounded in editor/LSP/diagnostic provenance;
- safe to explore without side effects;
- consistent across kinds through shared navigation and selection language;
- honest when data is missing, invalid, partial, stale, or unsupported.

The agent may decide that a callstack representation is useful, but it cannot
invent the interaction model. The frontend owns rendering and allowlisted local
intents. Keywords may suggest a Widget; they must not make the UI feel randomly
triggered or brittle.

An agent explanation embedded next to a real call node can be educational and
specific. An authoritative-looking fabricated graph destroys trust faster than a
plain `Call hierarchy unavailable` message.

## Trust model

Trust comes from boundaries that remain visible:

- **Intent boundary:** only PromptWindow introduces new intent; Accept can only
  continue a Goal that was already authorized.
- **Execution boundary:** only one turn runs; reopening PromptWindow interrupts
  it instead of creating concurrency.
- **Representation boundary:** AgentWindow shows work; Widgets cannot become
  arbitrary backend controls.
- **Context boundary:** selected references keep provenance and appear before
  submission.
- **Instruction boundary:** project facts are host-derived; Markdown Skills are
  bounded inert text with visible selection, provenance, and content hashes.
  Neither grants execution.
- **Tab boundary:** AgentWindow remains attached to the context where it began.
- **Mutation boundary:** the editable diff and `Accept` / `Reject` remain explicit;
  Widget selection and ordinary prompt submission do not mutate code. Accept may
  continue only a Goal already authorized by the user.
- **Uncertainty boundary:** unsupported, invalid, partial, timeout, truncated,
  denial, warning, and error states are named.

Any feature that looks faster by hiding one of these boundaries is a regression.

## Voice and copy

Runtime copy is English, sentence case, concise, and concrete.

Preferred patterns:

- state + fact: `Turn interrupted`, `Call hierarchy unavailable`;
- selection + scope: `3 files · 3 call sites selected`;
- object + local action: `Open call site`, `Clear selected context`;
- review + consequence: `Accept and continue goal`, `Reject and explain`;
- precise limitation + route: `Widget version unsupported`;
- honest provisional language: `validating response`;
- neutral agency: `Agent could not proceed` followed by the reason.

Avoid:

- general agent-command buttons in AgentWindow (`Draft`, `Retry`, `Goal`,
  `Follow`); Review's `Accept` / `Reject` is the intentional exception;
- praise, celebration, emojis, or anthropomorphic filler;
- vague busy language such as “Doing magic” or “Almost there”;
- blame when state drift or invalid data is the cause;
- raw backend jargon when an editor-level recovery can be named;
- wording that makes a local Widget selection sound like an executed action;
- marketing copy inside the work loop.

Concise must not become cryptic. Errors state what failed and the next safe route
when known. Warnings remain proportional to their actual consequence.

## Motion and pacing

- PromptWindow appears before optional warmup/context work completes.
- Project profiling begins in the Rust turn path only after composition and does
  not run external tools or block PromptWindow editing. Neovim contributes its
  already-active LSP facts without spawning another client.
- Agent progress updates in place and never spawns another product Window.
- The initial processing View appears directly at its stable source-relative
  geometry; later progress may change content and size, but not repair a missing
  anchor by teleporting the Window.
- Async AgentWindow updates do not steal focus.
- PromptWindow interruption restores authorship immediately while preventing a
  racing second turn.
- Reject restores authorship without invoking the model, keeps AgentWindow
  visible, and opens PromptWindow for an optional explanation.
- Widget exploration is instant and local whenever data is already present.
- Flow loading is incremental, batched, and explicit about partial data.
- Local-model reasoning, streaming, and bounded reads use short concrete phase
  messages. Private reasoning and raw tool payloads stay hidden; the user sees
  what evidence operation is happening, not a theatrical transcript.
- An expired provider response chain recovers once with a calm, explicit
  context-rebuild message instead of silently losing the conversation.
- Wrap, restore, source navigation, context selection, and context removal do not
  spend a model turn.
- Stop is immediate and avoids a ceremonial completion card.

Motion or streaming is justified only when it improves orientation, perceived
responsiveness, or control. Animation that merely makes the agent look busy is
out of character.

## Density and disclosure

Always visible when relevant:

- the agent's current content or processing state;
- whether AgentWindow is visible, wrapped, or off-tab;
- Widget provenance and uncertainty that affect interpretation;
- current local Widget selection;
- the summary of selected context before prompt submission;
- the summary of selected instruction Skills before prompt submission;
- errors that block safe continuation.
- during Review: the agent comment, exact proposal, target path, and
  `Accept` / `Reject` consequence.

Progressively disclosed:

- deeper Widget branches and exact uses;
- long explanations attached to nodes;
- full selected-context details;
- model choices, token/context accounting, and logs.

Metadata stays accurate and muted. It must not compete with the answer or turn
AgentWindow into a permanent control panel.

## Product tensions to preserve

### Two surfaces, not two limitations

Complex content is allowed. It must be expressed through Views and Widgets rather
than escaping into new Windows.

### Compact, not absent

Wrapped mode yields space while preserving identity and recovery. It must not
make the session appear lost.

### Interactive, not executable

Widgets can be rich and responsive. Their interaction remains local until the
user authors and submits a prompt.

Patch Review is not a Widget shortcut. It is a distinct mutation gate. Accept
continues prior authorization; Reject pauses without asking the model to react.

### Confident, not magical

Offer useful structure and explanations. Preserve provenance and name incomplete
knowledge.

Confidence means honoring the visible mode exactly. The agent does not guess
whether prose means investigation or implementation. When the user selects
`fix` or `propose`, it prepares the reviewed diff instead of inserting a Finding
detour. The diff remains inert until explicit `Accept`.

The more deterministic route stays one keystroke away. Mode is not hidden
configuration or prompt syntax: every PromptWindow states it in the title, and
`<C-k>` changes it without disturbing the sentence being written. Selecting
`fix` should feel like choosing an instrument before using it, not negotiating
with the agent after it misunderstood the request.

### Fast, not concurrent

Interrupt promptly and accept redirection. Never run overlapping turns to make
the interface appear faster.

### Technical, not dashboard-like

Expose facts where they help the current task. Avoid accumulating panels,
counters, action rows, and persistent chrome.

## Protected feeling invariants

- The user always knows which surface owns intent and which owns agent output.
- The code remains primary; two stable Windows feel temporary and local.
- Redirecting the agent feels safe because it cancels the old turn.
- Wrapped and off-tab work feels recoverable, not lost or omnipresent.
- Widget exploration feels consequence-free until the user submits a prompt.
- Patch acceptance feels explicit; rejection feels immediate, token-free, and
  recoverable through the still-visible AgentWindow and PromptWindow.
- New-file and directory review feels grounded in the actual parent directory,
  with nonexistent paths stated honestly rather than faked in Netrw.
- Selected context feels deliberate because it is visible and removable.
- Instruction selection feels deliberate because autoload is marked, optional
  files are explicitly chosen, and the selection lasts exactly one session.
- Structured explanations feel grounded in real editor data.
- Waiting feels observable and interruptible, not theatrical.
- Completion and Stop remain quiet.
- A direct implementation request feels understood: it reaches Patch Review
  without a ceremonial Finding detour, while genuine questions remain answers.
- The user can always name the current PromptWindow mode at a glance and change
  it without losing prompt text, context, or spatial continuity.

## Self-healing protocol

1. State the intended feeling delta before implementation using the axes above.
   If none is intended, protect them.
2. Evaluate the complete loop: prompt, handoff, processing, wrapping, tab change,
   response, Widget exploration, context attachment, diff review, Accept, Reject,
   Netrw creation context, next prompt, and Quit/Stop.
3. Look for popup fragmentation, hidden context, accidental backend actions,
   focus theft, parallel-turn ambiguity, fake provenance, or dashboard creep.
4. Rewrite current canonical language in place when intent changes. Do not append
   taste notes or a design changelog.
5. Reconcile `Known implementation gaps` in `ui.md` and `interactions.md` whenever
   shipped experience moves.
6. Re-read both companion contracts; feeling cannot be changed independently of
   concrete surfaces and behavior.

Primary reconciliation sources: the complete prompt-to-stop experience,
`README.md`, runtime copy under `lua/loopbiotic/`, protocol schemas, interaction
tests, `assets/loopbiotic.svg`, and the current demo. Prefer live behavior and
tests over an older recording.
