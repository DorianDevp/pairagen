# Pair

Pair is an interactive pair-programming stepper for Neovim.

It is not a chat.
It is not autocomplete.
It shows one strong hypothesis, finding, or patch at a time.
You follow, inspect, fix, apply, or stop.
Backends are replaceable.
The editor experience stays the same.

## Status

MVP core is implemented:

- Neovim labeled textarea prompt, card, navigation, annotation, diff, apply and reject UI
- thinking spinner, resume and reset controls
- session token usage and local error log
- JSON-RPC over stdio
- Rust session harness
- one-card state machine
- patch gate
- mock backend
- generic CLI backend

## Neovim Setup

```lua
require("pair").setup({
  backend = {
    command = "paird",
    args = { "--stdio" },
    agent = "mock",
    mode = "auto",
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

## Agents

```lua
require("pair").setup({
  backend = {
    command = "paird",
    args = { "--stdio" },
    agent = "codex",
  },
  agents = {
    codex = {
      kind = "generic",
      command = "codex",
      timeout = 180,
      args = {
        "exec",
        "--sandbox",
        "read-only",
        "--color",
        "never",
        "--skip-git-repo-check",
        "--output-schema",
        "/path/to/pairagen/schemas/pair-agent-op.schema.json",
        "-",
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
```

## Flow

```text
<leader>a
Prompt
Hypothesis
Follow, Why, Fix, Other, Stop
Patch
Apply or Reject
Summary
```

## Commands

```vim
:Pair
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
:PairBackend
:PairAgent
```

`PairLog` prints the log path. The default path is:

```text
~/.local/state/nvim/pairagen.log
```

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

The generic backend sends a strict JSON card contract to stdin and expects one JSON card on stdout.
