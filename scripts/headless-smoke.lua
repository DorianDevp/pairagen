local root = vim.fn.getcwd()
vim.opt.runtimepath:append(root)

local config = require("pair.config")
config.setup({
  backend = { agent = "codex", command = "/tmp/paird-test" },
  agents = {
    codex = {
      kind = "codex_app",
      command = "codex",
      model = "test-model",
      args = { "app-server", "--stdio" },
    },
  },
})
assert(config.values.backend.prefetch == "off")

local prompt = require("pair.prompt")
assert(prompt.title("Prompt") == " Pair Prompt · codex / test-model ")
prompt.open_for({ title = prompt.title("Reply"), footer = " Test ", submit = function() end })
local state = require("pair.state")
assert(vim.api.nvim_win_get_config(state.prompt_frame_win).zindex == 200)
assert(vim.api.nvim_win_get_config(state.prompt_win).zindex == 201)
prompt.close()

local context = require("pair.context")
local queries = context.workspace_queries("Replace preview_html using LayoutEditor template", 3)
assert(queries[1] == "preview_html")

local navigation = require("pair.navigation")
local location = navigation.card_location({
  evidence = { file = "old.rs" },
  next_move = { kind = "open_location", file = "templates/layout_editor.html" },
})
assert(location.file == "templates/layout_editor.html")

local card = require("pair.card")
local diff = require("pair.diff")
local long_goal = "Mam tutaj problem, bo ta pętla nie uwzględnia wszystkich elementów z kolejnych przebiegów"
local long_explanation = "Oznacza korzeń podglądu tym samym dyskryminatorem w węźle nadrzędnym i zachowuje pełny opis zmiany"
local patch_card = {
  kind = "patch",
  title = "Preview root",
  explanation = long_explanation,
}
require("pair.commands").setup()
assert(vim.fn.exists(":PairAssess") == 2)
state.goal = {
  statement = long_goal,
  completed_steps = { "first", "second" },
  known_observations = {},
  status = "active",
  next_step = "Przenieś model podglądu do rekurencyjnego enuma",
}
state.details_expanded = false
local compact = diff.control_lines(patch_card, config.values.keymaps)
assert(compact[1]:match("%.%.%.$"))
assert(table.concat(compact, "\n"):find("Expand details", 1, true))
assert(table.concat(compact, "\n"):find("Now   Przenieś model", 1, true))
assert(not table.concat(compact, "\n"):find(long_explanation, 1, true))

state.details_expanded = true
local expanded = diff.control_lines(patch_card, config.values.keymaps)
local expanded_text = table.concat(expanded, "\n")
assert(expanded[1] == "Goal  " .. long_goal)
assert(expanded_text:find(long_explanation, 1, true))
assert(expanded_text:find("Collapse details", 1, true))
assert(vim.fn.strchars(card.short("Zażółć gęślą jaźń i dłuższy opis", 18)) > 0)
state.goal = nil
state.details_expanded = false

local installer = require("pair.installer")
assert(installer.artifact("x86_64-unknown-linux-musl") == "paird-v0.1.0-x86_64-unknown-linux-musl.tar.gz")

local log = require("pair.log")
local sanitized = log.sanitize({ buffer_text = "secret source", event = "kept" })
assert(sanitized.buffer_text.redacted == true)
assert(sanitized.buffer_text.bytes > 0)
assert(sanitized.event == "kept")

print("Pairagen headless smoke test passed")
