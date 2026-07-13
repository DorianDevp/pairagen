Repo ready for desloppification. I had fun with vibing this project,
but because of growing size and complexity it's time take the steer.

# Pairagen

Pairagen is an interactive pair-programming stepper for Neovim.

It is not a chat.
It is not autocomplete.
It shows one strong hypothesis, finding, or patch at a time.
You follow, inspect, fix, apply, or stop.
Backends are replaceable.
The editor experience stays the same.

## Status and compatibility

Pairagen is beta software. It has been developed and tested primarily with the
Codex CLI app-server backend. Persistent Claude CLI (stream-json), Ollama HTTP,
generic CLI, and stdio agent adapters are available, but currently receive less
real-world testing than Codex.

Requirements:

- Neovim 0.10 or newer,
- `curl`, `tar`, and either `sha256sum` or `shasum` for managed installation,
- Codex CLI for the tested Codex backend,
- Linux x86_64/aarch64 or macOS Intel/Apple Silicon for managed `paird` binaries.

Implemented capabilities include:

- Neovim labeled textarea prompt, card, navigation, annotation, diff, apply and reject UI
- thinking spinner, resume and reset controls
- session token usage and local error log
- JSON-RPC over stdio
- Rust session harness
- one-card state machine
- patch gate
- mock backend
- generic CLI backend
- persistent Claude CLI backend (one stream-json process per session)
- Ollama HTTP backend for local models (model stays loaded, JSON-forced output)
- structured agent denial (`deny` op) rendered as a distinct card
- deterministic token-budgeted project context with LSP hints and dependency ranking

## Installation

With lazy.nvim:

```lua
{
  "DorianDevp/pairagen",
  config = function()
    require("pair").setup({
      backend = {
        agent = "codex",
      },
    })
  end,
}
```

On the first Pair request, the plugin downloads the matching versioned `paird`
archive from GitHub Releases, verifies its SHA-256 checksum, and installs it
under `stdpath("data")/pairagen/bin`. No global installation is required.

Run `:checkhealth pair` after installation.

### Manual backend

Automatic installation can be disabled or replaced with a custom binary:

```lua
require("pair").setup({
  backend = {
    command = "/absolute/path/to/paird",
    args = { "--stdio" },
    agent = "codex",
    mode = "auto",
  },
  distribution = {
    auto_install = false,
  },
})
```

For local development:

```lua
require("pair").setup({
  backend = {
    command = "cargo",
    args = { "run", "-p", "paird", "--", "--stdio" },
    agent = "mock",
    mode = "auto",
  },
})
```

When using a built `target/debug/paird`, run `cargo build -p paird` after protocol
changes. `cargo test` only refreshes test executables under
`target/debug/deps`. The client rejects stale `paird` protocol versions before
starting a session.

## Agents

```lua
require("pair").setup({
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
      command = "paird",
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
:PairAgent codex
:PairAgent agent
:PairAgent claude
:PairAgent local
:PairModel <model>
```

If the active agent has no `model` set in `setup()`, `:PairModel <model>` stores
the selection per agent in `stdpath("state")/pairagen/preferences.json` and
restores it on the next Neovim start. A model explicitly configured in
`setup()` always takes precedence. `:PairModel default` clears the stored model
and returns that agent to its own default.

## Flow

```text
<leader>a
Prompt
Persistent goal
Hypothesis
Follow, Why, Fix, Other, Stop
One local patch
Edit the inline draft
<leader>pa Accept, <leader>pd Reject, <leader>pr Retry
Accepted local step
Local applied receipt
Explicit Next, Check, or Stop
Next patch (only when requested)
One local patch or completed goal summary
```

Cards stay anchored beside the source line and do not take focus. Use `<leader>pg`
to jump to a finding or the first line of an inline draft, and `<leader>pr` to
focus the current Pair card.

By default the first card is whatever fits the prompt best: a hypothesis, a
finding, or a clarifying choice when the prompt is ambiguous. Start the prompt
with `/{kind}` to demand a specific card instead — `/hypothesis`, `/finding`,
`/patch` (alias `/fix`), `/choice`, or `/summary`. For example
`/patch guard the payload here` skips discovery and drafts a patch directly.
Unknown words after `/` are treated as normal prompt text, so paths like
`/tmp/project` are safe.

The goal and accepted-step count stay visible on cards and editable drafts. After
accepting a patch, Pair shows a local receipt without calling the agent again.
`Next` explicitly continues the same goal and must return either one local patch
or a completed-goal summary; it does not restart discovery. This keeps the user
in control and avoids spending an entire model turn merely to ask whether to
continue.

## Context optimization

Pair builds a small ranked context bundle before calling an agent. The current
buffer around the cursor remains the source of truth for editable patches. Extra
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
`min_artifact_score` are omitted. The project index is incremental, cached in the `paird`
process and invalidated after an applied Pair patch. Ranked fragments are packed
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
thread, but accepting a patch rotates the patch thread before the next local
step so accumulated conversation history does not grow without bound.

The defaults can be overridden during setup:

```lua
require("pair").setup({
  prompt = {
    -- Prompt and reply windows stay above Pair cards.
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
})
```

Each card shows the used context budget and selected fragment count. The JSONL
trace contains a `context_optimization` event with cache statistics, ranked
candidates, scores and selection decisions. It does not add those statistics to
the agent prompt.

Choosing `Fix` on a card first moves the source context to the card's
`next_move` (falling back to evidence/location), then captures the next request.
The patch agent therefore receives the recommended consumer or template instead
of the file where discovery happened.

The future optional classical-ML ranking design is documented in [`ml.md`](ml.md).
The current implementation does not train or run an ML model.

## Commands

```vim
:Pair
:PairReply
:PairFix
:PairWhy
:PairFollow
:PairOther
:PairNext
:PairStop
:PairHide
:PairResume
:PairReset
:PairLog
:PairLogClear
:PairBackend
:PairAgent
:PairModel
```

`:PairLog` prints the current JSONL session trace. It records the backend command
and protocol handshake, structured RPC requests/responses, progress events,
cards, goals, token usage, and backend errors. Every completed backend turn also
emits an `agent_attempts` event. Each attempt records its
accepted/retry/rejected outcome, retry metadata, per-attempt token usage and
compact tool activity. Content-bearing fields are represented by redaction
metadata unless full-content logging is explicitly enabled.

The default trace location is:

```text
~/.local/state/nvim/pairagen/sessions/<timestamp>-<pid>.jsonl
```

Logs redact prompts, source excerpts, diffs, findings, and model content by
default. At most 20 trace files are retained. Logging can be disabled or full
content can be enabled explicitly:

```lua
require("pair").setup({
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
:checkhealth pair
```

It reports the plugin and protocol versions, release target, managed `paird`
state, downloader prerequisites, active agent/model, logging privacy, and LSP
clients. A protocol mismatch means the Lua plugin and `paird` come from
different releases; remove the managed version directory or update the plugin.

## License

Pairagen is available under the [MIT License](LICENSE).

## paird

```bash
paird --stdio
paird backend list
paird backend check
paird schema card
paird dev mock-session
```

## Generic Backend

```bash
PAIR_BACKEND=generic \
PAIR_GENERIC_COMMAND=codex \
paird --stdio
```

`PAIR_GENERIC_ARGS` is split on whitespace.

The generic backend sends a strict JSON card contract to stdin. It accepts a raw final JSON card for backwards compatibility, or an NDJSON stream:

```json
{"t":"pair_progress","phase":"reviewing","message":"Reviewing the supplied context"}
{"t":"pair_result","result":{"op":"hypothesis","title":"...","claim":"..."}}
```

`pair_progress.message` is user-visible feedback. It must be a concise status summary, never raw model reasoning. Claude and local agents that do not emit this protocol still show lifecycle feedback from Pair while their process is running.
