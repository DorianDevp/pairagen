<p align="center">
  <img src="assets/loopbiotic.svg" width="96" alt="Loopbiotic logo">
</p>
<p align="center"><strong>WHERE HUMAN IS IN THE LOOP</strong></p>

Repo ready for desloppification. I had fun with vibing this project,
but because of growing size and complexity it's time take the steer.

# Loopbiotic

![Loopbiotic demo](assets/loopbiotic.gif)

Loopbiotic is an interactive pair-programming stepper for Neovim.

It is not a chat.
It is not autocomplete.
It shows one strong hypothesis, finding, or patch at a time.
You follow, inspect, fix, apply, or stop.
Backends are replaceable.
The editor experience stays the same.

## Status and compatibility

Loopbiotic is beta software. It has been developed and tested primarily with the
Codex CLI app-server backend. Persistent Claude CLI (stream-json), Ollama HTTP,
an OpenAI-compatible HTTP adapter used for local LM Studio benchmarks, generic
CLI, and stdio agent adapters are available, but currently receive less
real-world testing than Codex.

Requirements:

- Neovim 0.10 or newer,
- `curl`, `tar`, and either `sha256sum` or `shasum` for managed installation,
- Codex CLI for the tested Codex backend,
- Linux x86_64/aarch64 or macOS Intel/Apple Silicon for managed `loopbioticd` binaries.

Implemented capabilities include:

- Neovim labeled textarea prompt, card, navigation, annotation, diff, apply and reject UI
- thinking spinner, resume and reset controls
- raw, cached, and non-cached session token usage plus a local error log
- JSON-RPC over stdio
- Rust session harness
- explicit per-PromptWindow modes with local hunk review
- patch gate
- mock backend
- generic CLI backend
- persistent Claude CLI backend (one stream-json process per session)
- Ollama HTTP backend for local models (model stays loaded, JSON-forced output)
- stateful OpenAI-compatible Responses backend for LM Studio, with SSE progress,
  optional reasoning, and bounded read-only workspace tools
- structured agent denial (`deny` op) rendered as a distinct card
- deterministic token-budgeted project context with LSP hints and dependency ranking

## Installation

With lazy.nvim:

```lua
{
  "DorianDevp/loopbiotic",
  config = function()
    require("loopbiotic").setup({
      backend = {
        agent = "codex",
      },
    })
  end,
}
```

On the first Loopbiotic request, the plugin downloads the matching versioned `loopbioticd`
archive from GitHub Releases, verifies its SHA-256 checksum, and installs it
under `stdpath("data")/loopbiotic/bin`. No global installation is required.

Run `:checkhealth loopbiotic` after installation.

### Manual backend

Automatic installation can be disabled or replaced with a custom binary:

```lua
require("loopbiotic").setup({
  backend = {
    command = "/absolute/path/to/loopbioticd",
    args = { "--stdio" },
    agent = "codex",
    mode = "investigate",
  },
  distribution = {
    auto_install = false,
  },
})
```

For local development:

```lua
require("loopbiotic").setup({
  backend = {
    command = "cargo",
    args = { "run", "-p", "loopbioticd", "--", "--stdio" },
    agent = "mock",
    mode = "investigate",
  },
})
```

When using a built `target/debug/loopbioticd`, run `cargo build -p loopbioticd` after protocol
changes. `cargo test` only refreshes test executables under
`target/debug/deps`. The client rejects stale `loopbioticd` protocol versions before
starting a session.

## Agents

```lua
require("loopbiotic").setup({
  backend = {
    agent = "codex",
  },
  agents = {
    codex = {
      kind = "codex_app",
      command = "codex",
      args = {
        "app-server",
        "--stdio",
      },
    },
    agent = {
      kind = "agent",
      command = "loopbioticd",
      args = { "dev", "stdio-agent" },
    },
    claude = {
      kind = "generic",
      command = "claude",
      args = {},
    },
    ["local"] = {
      kind = "generic",
      command = "ollama",
      args = { "run", "qwen2.5-coder:7b" },
    },
  },
})
```

Switch at runtime:

```vim
:LoopbioticAgent codex
:LoopbioticAgent agent
:LoopbioticAgent claude
:LoopbioticAgent local
:LoopbioticModel <model>
:LoopbioticDiscoveryModel <model>
```

If the active agent has no `model` set in `setup()`, `:LoopbioticModel <model>` stores
the selection per agent in `stdpath("state")/loopbiotic/preferences.json` and
restores it on the next Neovim start. `:LoopbioticDiscoveryModel <model>` does the
same for the discovery model (investigate/explain/review turns), stored
separately per agent. A model explicitly configured in `setup()` always takes
precedence. `:LoopbioticModel default` (or `:LoopbioticDiscoveryModel default`)
clears the stored model and returns that agent to its own default.

The prompt window title always names the selected mode, active agent, and the
concrete model the next turn will use, e.g. `fix · codex / gpt-…`. Without a configured
model it shows the model the backend announces during warmup (or reported
after the last turn), and `model?` until one is known — it never shows
`default`. The title names the model the turn actually runs: a patch mode
(`fix`/`propose`) shows the patch-drafting model, a discovery mode
(`explain`/`investigate`/`review`) shows the discovery model (the shipped Codex
agent uses `gpt-5.4-mini` at low effort; Claude pins `discovery_model = "haiku"`).
Press `<C-l>` (`keymaps.models`) inside the prompt to pick a model from every
known candidate: the configured patch and discovery models, the models the
backend enumerates (Codex `model/list`, Ollama's local tags; claude offers its
stable CLI aliases `sonnet`, `opus`, `haiku`), an optional `models` list on the
agent definition, and the model reported by the last turn. The picker sets the
model for the current mode's phase — `<C-l>` in `fix`/`propose` sets the patch
model, in the other modes it sets the discovery model (`:LoopbioticModel` and
`:LoopbioticDiscoveryModel` do the same from the command line). The picked model
persists per agent; the prompt window and its typed text stay open.

Every PromptWindow, including Reply, also shows exactly one turn mode in its
title. Press `<C-k>` (`keymaps.modes`) inside the prompt to choose `fix`,
`explain`, `investigate`, `review`, or `propose`. The picker preserves typed text
and attached context and sends nothing until submit. Both a new session and a
Reply transmit the mode visible at submit time.

```lua
require("loopbiotic").setup({
  agents = {
    ["local"] = {
      kind = "ollama",
      host = "http://127.0.0.1:11434",
      models = { "qwen2.5-coder:7b", "llama3.1:8b" }, -- extra picker candidates
    },
  },
  keymaps = {
    skills = "<C-g>", -- session Markdown multiselect above PromptWindow
    modes = "<C-k>", -- mode picker inside every prompt window
    models = "<C-l>", -- model picker inside the prompt window
  },
  skills = {
    autoload = { "AGENTS.md" }, -- locked, session-scoped instructions
    discover_root_markdown = true,
    max_file_bytes = 65536,
    picker_height = 10,
  },
})
```

## Project Intelligence and Skills

Loopbiotic gives every backend a deterministic `ProjectProfile` alongside the
ranked source context. Lua only contributes cheap facts from already-active
Neovim LSP clients. On session start, a Rust registry activates adapters from
root markers such as `deno.json`, `package.json`, `nx.json`, `Cargo.toml`, and
Compose files. Root facts are read concurrently, matching adapters run in
parallel, and their results are merged deterministically. No adapter knows a
project name.

The first POC includes independent adapters for package workspaces, TypeScript,
Angular, React, Excalidraw, RxJS, Deno, Nx, Cargo/Rust, Axum, SQLx, Tokio,
Docker Compose, and Neovim LSP. It records the adapter IDs that fired, exact
versions from `deno.lock`, Nx project areas and dependencies, Cargo workspace
members, project tasks, selected runtime/infrastructure versions, and bounded
LSP capabilities. Profiling runs without an agent turn, command execution,
network request, or MCP. This keeps frontier models on a direct evidence path
while giving smaller local models facts they are less likely to infer reliably.

The reproducible real-model A/B runner compares no profile/Skills, profile only,
and profile plus selected Skills across the fixtures in
`tests/fixtures/project-intelligence`. See the
[2026-07-20 benchmark](doc/benchmarks/project-intelligence-2026-07-20.md) for
methodology and measured tradeoffs, or run:

```sh
scripts/project-intelligence-report.sh --repeat 3 --out results.jsonl
```

Inspect adapter activation and the exact profile during development with:

```sh
cargo run -q -p loopbioticd -- dev project-profile ../libregraf
```

Press `<C-g>` (`keymaps.skills`) in PromptWindow or Reply to open a bounded
multiselect Frame above the prompt. It lists Markdown files in the workspace
root and configured `skills.autoload` paths. Space toggles an optional file,
Enter applies the selection, and Escape cancels it. Autoloaded files are marked
`auto` and cannot be deselected. The selected filenames stay visible in the
PromptWindow footer and persist until Stop or Reset.

On submit, Loopbiotic snapshots each selected file as workspace-relative inert
text with provenance and a SHA-256 content hash. Selection never executes the
file, grants tools, or contacts the backend by itself. Protocol limits bound the
number, individual size, and combined size of instruction files. Exact project
versions ground the model; versioned framework knowledge packs and native
compile probes—for example Angular 22 and TypeScript 6 guidance for an older
local model—remain follow-up work.

The LM Studio backend adds a separate Rust-owned evidence loop for discovery and
explicit Goal turns. It can perform bounded workspace-relative file reads,
literal search, and directory listing without MCP, command execution, network
access, or source mutation. Ordinary Patch turns receive no read tools. Provider
response IDs keep Replies stateful, while an expired chain is rebuilt once from
the current bounded session context. Reasoning text remains private; AgentWindow
shows only concise reasoning, streaming, read, and recovery phases.

## Flow

```text
<leader>pp
PromptWindow → submit one intent
AgentWindow → Working → response or Widget
Patch → editable diff plus Accept / Reject
Accept → continue the authorized goal to its next review boundary
Reject → restore source, pause, keep AgentWindow, open PromptWindow
```

Loopbiotic has exactly two product surfaces. PromptWindow owns user intent;
AgentWindow owns progress, responses, review controls, and Widgets. AgentWindow
never steals focus on async updates and remains attached to the tab where the
session began. `<leader>ph` wraps it in the upper-right, while `<leader>pr`
returns to its owner tab and restores the full View. `<leader>pg` performs local
source navigation. Long goal and review explanations use `z` for local detail.

Conversational turns have a 10-second interaction deadline and work turns have
a 20-second deadline. Crossing it updates AgentWindow to a non-actionable
`Working` View instead of holding Neovim. Opening PromptWindow during this state
invalidates the frontend generation, cancels the real backend turn, and blocks
submission until cancellation settles. Slow-turn timing is recorded in the local trace and a
compact, content-free instruction is injected into the next turn so the agent
prioritizes an earlier useful response.

For an explicit goal, each backend turn may return at most one file, one
uninterrupted change block, and 32 changed lines, plus a plan of the remaining
coherent steps. A single `@@` containing changed lines separated by unchanged
context is still a batch and is rejected as multiple hunks. The next hunk may
be prepared while the current one is reviewed.
Accepting normally surfaces the next review boundary immediately. Rejecting is
token-free: it restores accepted source, pauses the goal, changes AgentWindow to
Reply/Quit, and opens PromptWindow for the user's explanation. No replacement is
generated automatically. Source navigation stays inside the workspace, and
every edit remains an inert draft until accepted.

Compiler acceptance is an invariant at every review boundary. Applying one
proposed patch to the currently accepted source must leave the project compiling
and type-checking without relying on a later patch. Dependency-producing work is
ordered first: declarations, interfaces, types, imports, fields, functions, and
compatibility shims must exist in an independently valid patch before a later
patch references or implements them. For interface extraction this means one
patch introduces the valid interface declaration; only after it is accepted may
another patch replace the inline type or implement that interface. If no valid
intermediate state fits one uninterrupted block, including the project's unused
declaration checks, the agent must stop for a real decision instead of returning
broken code or a batch.

Location-bearing responses do not move the editor asynchronously. Use the local
Go-to control to open their evidence. Draft review moves to the proposed hunk;
AgentWindow retains its original tab ownership.

There is no automatic intent-routing mode. The mode visible in PromptWindow is
the contract: `fix` and `propose` require a reviewed patch; `explain`,
`investigate`, and `review` require their non-mutating response kinds. The safe
configured default is `investigate`. Slash-prefixed text is ordinary prompt
content and cannot override the visible mode.

The goal and accepted-step count remain subordinate metadata. New intent always
comes from PromptWindow; AgentWindow does not expose Draft, Follow, Why, Goal,
Retry, or Cancel actions. Review's Accept/Reject pair is the sole deliberate
exception because it resolves a pending source mutation.

By default `backend.prefetch = "read_only"` prepares the next goal step while an
ordinary draft is reviewed. It remains inert and can surface only after that
draft is accepted. Set it to `"off"` to disable this.

Cards show raw, cached, and non-cached turn and session usage against
`backend.token_budget` (50,000 raw tokens by default).
After the budget is reached, Loopbiotic asks before every additional agent turn; local
navigation, patch review/apply, and stopping remain immediate. Set the budget to
`0` to disable this guard.

## Context optimization

Loopbiotic builds a small ranked context bundle before calling an agent. Live editor
buffers remain the source of truth when validating editable patches. Extra
project fragments are selected deterministically from:

- definitions, declarations, type definitions and implementations reported by
  active Neovim LSP clients,
- diagnostics published through `vim.diagnostic`, including diagnostics exposed
  by tools such as rust-analyzer and clippy,
- direct and two-hop import/module dependencies,
- symbol definitions and references matching the prompt, selection and cursor,
- prompt-driven workspace symbols from the user's attached language servers,
- related tests.

Generated directories, VCS metadata, dependency vendors and large or binary
files are excluded. Source-like templates and assets (including HTML, CSS,
Askama/Jinja/Handlebars/Tera/Twig templates, Astro and GraphQL) are indexed too.
An exact prompt path or basename receives the strongest deterministic signal;
rare compound identifiers such as `preview_html` are favored while terms found
throughout the repository are down-ranked. Candidates below
`min_artifact_score` are omitted. The project index is incremental, cached in the `loopbioticd`
process and invalidated after an applied Loopbiotic patch. Ranked fragments are packed
into a hard token budget; candidates which do not fit are omitted.

Cursor LSP queries and prompt-driven `workspace/symbol` queries share small,
configurable deadlines. Results outside the project root are discarded and
duplicate locations from multiple language servers or methods are merged. This
lets existing clients such as typescript-language-server, Angular LS, gopls,
Intelephense and rust-analyzer act as a cheap semantic index. Diagnostics from
clippy remain available through `vim.diagnostic`.

Codex app-server threads also fingerprint their supplied context. An unchanged
buffer and unchanged ranked fragments are referenced from the preceding turn
instead of being sent again. Stateless generic and stdio backends continue to
receive a complete compact bundle. Contract retries reuse the current Codex
thread. Reviewing queued hunks is entirely local, so it does not extend the
agent conversation or resend the goal context.

### Flow Widget

When the attached language server supports LSP call hierarchy, opening a
Loopbiotic prompt starts a non-blocking Flow lookup for the symbol under the
session cursor. The pinned graph initially follows callers and callees to depth
two, deduplicates overlapping clients, keeps every concrete call-site, and
keeps non-call references separate. The lookup never changes the prompt's
geometry or focus. Flow renders only as a Widget in AgentWindow and `F`
(`keymaps.flow`) toggles it there. When open, `j`/`k` select,
`h`/`l` fold or load a branch, `Enter` opens a definition, `u` lists exact
uses, `s` selects or deselects prompt context, and `R` explicitly roots the
graph at the current source cursor.

Wide viewports compose Flow beside the answer inside AgentWindow; narrow
viewports stack it in the same surface. When a
user asks the agent for a callstack or code-flow explanation, the agent returns
an ordered `flow_path` of existing LSP node IDs. The answer card renders that
focused path immediately; keys `1` through `9` open its displayed symbols,
while `F` opens the complete explorer. A
missing `callHierarchyProvider` is reported as `Call hierarchy unavailable`;
Loopbiotic never asks the agent to recreate the graph. The complete graph
available at submit time is included in `ContextBundle`. Selected files, symbols,
and call sites appear visibly in PromptWindow's footer, can be removed with
`Ctrl-x`, and reach the backend only on prompt submission.

The defaults can be overridden during setup:

```lua
require("loopbiotic").setup({
  keymaps = {
    modes = "<C-k>", -- mode picker inside every prompt window
    flow = "F", -- normal-mode Flow explorer toggle
  },
  prompt = {
    -- Prompt and reply windows stay above Loopbiotic cards.
    zindex = 200,
  },
  context = {
    before = 24,
    after = 24,
    optimization = {
      enabled = true,
      total_token_budget = 2400,
      reserved_tokens = 700,
      primary_token_budget = 1000,
      max_artifacts = 4,
      snippet_lines = 10,
      max_scan_files = 2000,
      max_file_bytes = 524288,
      cache_ttl_ms = 1500,
      min_artifact_score = 40,
      exclude = { "generated", "fixtures/large" },
    },
    lsp = {
      enabled = true,
      -- This is one total deadline shared by every active client and method.
      timeout_ms = 120,
      max_locations = 16,
      workspace_timeout_ms = 120,
      max_workspace_queries = 3,
      definition = true,
      declaration = true,
      type_definition = true,
      implementation = true,
      -- References can be expensive and numerous, so they are opt-in.
      references = false,
      workspace_symbols = true,
    },
  },
  flow = {
    enabled = true,
    initial_depth = 2,
    max_nodes = 40,
    snippet_token_budget = 800,
    -- An opened Flow explorer switches from split to a single-pane view below this width.
    responsive_split = 120,
    panel_width = 52,
    request_timeout_ms = 1200,
    -- Submit waits at most this long for requests already in flight.
    submit_wait_ms = 160,
  },
})
```

Each card shows the used context budget and selected fragment count. The JSONL
trace contains a `context_optimization` event with cache statistics, ranked
candidates, scores and selection decisions. It does not add those statistics to
the agent prompt.

`:LoopbioticFix` opens PromptWindow in fix mode. It never launches work directly
from AgentWindow; the user still reviews and submits the prompt snapshot.

The future optional classical-ML ranking design is documented in [`doc/ml.md`](doc/ml.md).
The current implementation does not train or run an ML model.

## Commands

```vim
:Loopbiotic
:LoopbioticReply
:LoopbioticFix
:LoopbioticWhy
:LoopbioticStop
:LoopbioticHide
:LoopbioticResume
:LoopbioticReset
:LoopbioticLog
:LoopbioticLogClear
:LoopbioticBackend
:LoopbioticAgent
:LoopbioticModel
```

`:LoopbioticLog` prints the current JSONL session trace. It records the backend command
and protocol handshake, structured RPC requests/responses, progress events,
cards, goals, token usage, and backend errors. Every completed backend turn also
emits an `agent_attempts` event. Each attempt records its
accepted/retry/rejected outcome, retry metadata, per-attempt token usage and
compact tool activity. Content-bearing fields are represented by redaction
metadata unless full-content logging is explicitly enabled.

The default trace location is:

```text
~/.local/state/nvim/loopbiotic/sessions/<timestamp>-<pid>.jsonl
```

Logs redact prompts, source excerpts, diffs, findings, and model content by
default. At most 20 trace files are retained. Logging can be disabled or full
content can be enabled explicitly:

```lua
require("loopbiotic").setup({
  logging = {
    enabled = true,
    include_content = false,
    max_files = 20,
  },
})
```

Full-content logs may contain proprietary code and should never be attached to
public issues without review.

## Troubleshooting

Run:

```vim
:checkhealth loopbiotic
```

It reports the plugin and protocol versions, release target, managed `loopbioticd`
state, downloader prerequisites, active agent/model, logging privacy, and LSP
clients. A protocol mismatch means the Lua plugin and `loopbioticd` come from
different releases; remove the managed version directory or update the plugin.

## License

Loopbiotic is available under the [MIT License](LICENSE).

## loopbioticd

```bash
loopbioticd --stdio
loopbioticd backend list
loopbioticd backend check
loopbioticd schema card
loopbioticd dev mock-session
```

## Generic Backend

```bash
LOOPBIOTIC_BACKEND=generic \
LOOPBIOTIC_GENERIC_COMMAND=codex \
loopbioticd --stdio
```

`LOOPBIOTIC_GENERIC_ARGS` is split on whitespace.

The generic backend sends a strict JSON card contract to stdin. It accepts a raw final JSON card for backwards compatibility, or an NDJSON stream:

```json
{"t":"loopbiotic_progress","phase":"reviewing","message":"Reviewing the supplied context"}
{"t":"loopbiotic_result","result":{"op":"hypothesis","title":"...","claim":"..."}}
```

`loopbiotic_progress.message` is user-visible feedback. It must be a concise status summary, never raw model reasoning. Claude and local agents that do not emit this protocol still show lifecycle feedback from Loopbiotic while their process is running.

## LM Studio / OpenAI-compatible Responses Backend

This backend requires a server that implements the OpenAI-compatible Responses
API with streaming and stored responses. LM Studio is the tested target:

```bash
LOOPBIOTIC_BACKEND=lm_studio \
LOOPBIOTIC_OPENAI_MODEL=qwen/qwen3.6-35b-a3b \
LOOPBIOTIC_OPENAI_BASE_URL=http://127.0.0.1:1234/v1 \
loopbioticd --stdio
```

`LOOPBIOTIC_OPENAI_MAX_TOKENS` defaults to `4096`.
`LOOPBIOTIC_OPENAI_TOOLS` defaults to `true`; read tools are still restricted to
discovery and explicit Goal turns. `LOOPBIOTIC_OPENAI_MAX_TOOL_CALLS` defaults
to `2` and is capped at `4`. `LOOPBIOTIC_OPENAI_REASONING_EFFORT` accepts
`none`, `minimal`, `low`, `medium`, `high`, or `xhigh` and defaults to `none`,
because some small models spend their entire output budget on hidden reasoning.
`LOOPBIOTIC_OPENAI_API_KEY` is optional. The shared
`LOOPBIOTIC_TURN_TIMEOUT_SECS` deadline and real turn cancellation also apply.
