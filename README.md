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

- Neovim prompt, card, navigation, annotation, diff, apply and reject UI
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
      args = {
        "exec",
        "--sandbox",
        "read-only",
        "--ask-for-approval",
        "never",
        "--color",
        "never",
        "--skip-git-repo-check",
        "--output-schema",
        "/path/to/pairagen/schemas/pair-agent-op.schema.json",
        "-",
      },
    },
    claude = {
      kind = "generic",
      command = "claude",
      args = {},
    },
    ["local"] = {
      kind = "api",
      base_url = "http://127.0.0.1:11434/v1",
      model = "qwen2.5-coder:7b",
    },
    openai = {
      kind = "api",
      base_url = "https://api.openai.com/v1",
      model = "gpt-4.1",
      api_key_env = "OPENAI_API_KEY",
    },
  },
})
```

Switch at runtime:

```vim
:PairAgent codex
:PairAgent claude
:PairAgent local
:PairAgent openai
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
:PairStop
:PairBackend
:PairAgent
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
