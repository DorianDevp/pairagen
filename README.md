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
    mode = "auto",
  },
})
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
