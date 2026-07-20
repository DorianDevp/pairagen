local root = vim.fn.getcwd()
vim.opt.runtimepath:append(root)

local config = require("loopbiotic.config")
require("loopbiotic").setup({
  backend = { agent = "codex", command = "/tmp/loopbioticd-test" },
  agents = {
    codex = {
      kind = "codex_app",
      command = "codex",
      model = "test-model",
      args = { "app-server", "--stdio" },
    },
  },
})
assert(config.values.backend.prefetch == "read_only")
assert(config.values.backend.token_budget == 50000)
assert(config.values.keymaps.resume == "<leader>pr")
assert(config.values.keymaps.draft_retry == nil)

local ui = require("loopbiotic.ui")
local below_row = select(1, ui.near({ row = 5, col = 20 }, 20, 4, 2, { width = 80, height = 20 }, 1))
local above_row = select(1, ui.near({ row = 18, col = 20 }, 20, 4, 2, { width = 80, height = 20 }, 1))
assert(below_row == 6)
assert(above_row == 10)

local legacy_preferences = vim.fn.stdpath("state") .. "/pairagen/preferences.json"
vim.fn.delete(config.preferences_path())
vim.fn.mkdir(vim.fn.fnamemodify(legacy_preferences, ":h"), "p")
vim.fn.writefile({ vim.json.encode({ models = { codex = "legacy-model" } }) }, legacy_preferences)
assert(config.read_preferences().models.codex == "legacy-model")
vim.fn.delete(legacy_preferences)

local prompt = require("loopbiotic.prompt")
assert(prompt.title("Prompt", "investigate") == " Loopbiotic Prompt · investigate · codex / test-model ")
prompt.open_for({ title = prompt.title("Reply"), footer = " Test ", submit = function() end })
local state = require("loopbiotic.state")
assert(vim.api.nvim_win_get_config(state.surfaces.prompt.frame_win).zindex == 200)
assert(vim.api.nvim_win_get_config(state.surfaces.prompt.win).zindex == 201)
prompt.close()
state.token_usage = { total_tokens = 50000 }
assert(require("loopbiotic").token_budget_exceeded())
state.token_usage = nil
assert(require("loopbiotic").workspace_location("README.md"))
assert(not require("loopbiotic").workspace_location("/tmp/outside-loopbiotic.txt"))

local context = require("loopbiotic.context")
local queries = context.workspace_queries("Replace preview_html using LayoutEditor template", 3)
assert(queries[1] == "preview_html")
local new_file = context.new_file("src/Exception/NewException.php")
assert(new_file.buffer_text == "")
assert(new_file.buffer_start_line == 1)
local current_file = context.file("scripts/headless-smoke.lua")
assert(current_file.file == "scripts/headless-smoke.lua")
assert(current_file.buffer_start_line == 1)
assert(current_file.buffer_text:find("Loopbiotic headless smoke test passed", 1, true))

local apply = require("loopbiotic.apply")
local new_lines = apply.apply_diff({ "" }, "@@ -1,0 +1,2 @@\n+<?php\n+final class NewException {}\n")
assert(new_lines[1] == "<?php")
assert(new_lines[2] == "final class NewException {}")

-- Whitespace-tolerant apply: context/remove lines drift on indentation and
-- trailing space, but still match; the buffer's real indentation is preserved.
local drifted = apply.apply_diff({ "\tguard", "\told" }, "@@ -1,2 +1,2 @@\n guard  \n-    old\n+    new\n")
assert(drifted[1] == "\tguard")
assert(drifted[2] == "    new")

local state = require("loopbiotic.state")
state.turn_token_usage = { total_tokens = 100, input_tokens = 90, cached_input_tokens = 80, output_tokens = 10 }
state.token_usage = vim.deepcopy(state.turn_token_usage)
state.backend_model = "claude-opus-4-8"
local token_lines = {}
require("loopbiotic.card").tokens(token_lines)
local token_text = table.concat(token_lines, "\n")
assert(token_text:find("in 90 (80 cached) · out 10", 1, true))
-- Billing: 10 fresh input @ $5/1M + 80 cached @ $0.50/1M + 10 output @ $25/1M
-- = 0.00005 + 0.00004 + 0.00025 = $0.00034, shown as $0.0003 at 4 decimals
assert(token_text:find("$0.0003", 1, true))
local pricing = require("loopbiotic.pricing")
assert(math.abs(pricing.cost(state.turn_token_usage, "claude-opus-4-8") - 0.00034) < 1e-9)
assert(pricing.cost(state.turn_token_usage, "some-unknown-model") == nil)
state.turn_token_usage = nil
state.token_usage = nil
state.backend_model = nil

local navigation = require("loopbiotic.navigation")
local location = navigation.card_location({
  evidence = { file = "old.rs" },
  next_move = { kind = "open_location", file = "templates/layout_editor.html" },
})
assert(location.file == "templates/layout_editor.html")

local card = require("loopbiotic.card")
local diff = require("loopbiotic.diff")
local change_cursor = diff.change_cursor({ "before", "  inserted", "after" }, {
  first_row = 0,
  added = { 1 },
})
assert(change_cursor[1] == 2)
assert(change_cursor[2] == 2)
local loopbiotic = require("loopbiotic")
local long_goal = "Mam tutaj problem, bo ta pętla nie uwzględnia wszystkich elementów z kolejnych przebiegów"
local long_explanation =
  "Oznacza korzeń podglądu tym samym dyskryminatorem w węźle nadrzędnym i zachowuje pełny opis zmiany"
local patch_card = {
  kind = "patch",
  title = "Preview root",
  explanation = long_explanation,
}
require("loopbiotic.commands").setup()
assert(vim.fn.exists(":Loopbiotic") == 2)
assert(vim.fn.exists(":LoopbioticFix") == 2)
assert(vim.fn.exists(":LoopbioticStop") == 2)
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

local installer = require("loopbiotic.installer")
assert(installer.artifact("x86_64-unknown-linux-musl") == "loopbioticd-v0.3.2-x86_64-unknown-linux-musl.tar.gz")

local log = require("loopbiotic.log")
local sanitized = log.sanitize({ buffer_text = "secret source", event = "kept" })
assert(sanitized.buffer_text.redacted == true)
assert(sanitized.buffer_text.bytes > 0)
assert(sanitized.event == "kept")

vim.cmd("edit README.md")
vim.cmd("tabedit CHANGELOG.md")
local navigation_tab = vim.api.nvim_get_current_tabpage()
assert(navigation.open_location({ file = "README.md", line = 2, column = 1 }))
assert(vim.api.nvim_get_current_tabpage() == navigation_tab)
assert(vim.fn.fnamemodify(vim.api.nvim_buf_get_name(0), ":t") == "README.md")

vim.cmd("tabonly")

print("Loopbiotic headless smoke test passed")

-- Optional real round-trip: when LOOPBIOTIC_SMOKE_BIN points at a loopbioticd
-- binary, drive one session over JSON-RPC against the mock agent. Without the
-- variable the smoke test keeps the stubbed behavior above and stops here.
local smoke_bin = vim.env.LOOPBIOTIC_SMOKE_BIN
if smoke_bin and smoke_bin ~= "" then
  local loopbiotic = require("loopbiotic")
  local rpc = require("loopbiotic.rpc")

  assert(vim.fn.executable(smoke_bin) == 1, "LOOPBIOTIC_SMOKE_BIN is not executable: " .. smoke_bin)
  loopbiotic.reset()
  config.values.backend.command = vim.fn.fnamemodify(smoke_bin, ":p")
  config.values.backend.args = { "--stdio" }
  config.values.backend.agent = "mock"

  vim.cmd("edit README.md")
  loopbiotic.submit_prompt("Smoke round-trip: inspect this file", "investigate")
  local started = vim.wait(15000, function()
    return state.session_id ~= nil and state.card ~= nil and state.card.kind ~= "working"
  end, 50)
  assert(started, "smoke round-trip timed out waiting for the first card")
  assert(state.card.kind == "hypothesis", "unexpected first card kind: " .. tostring(state.card.kind))

  local first_card_id = state.card.id
  loopbiotic.submit_reply("Review the evidence", "review", {})
  local replied = vim.wait(15000, function()
    return state.card ~= nil and state.card.id ~= first_card_id and not state.thinking_request_id
  end, 50)
  assert(replied, "smoke round-trip timed out waiting for the reply card")
  assert(state.card.kind == "finding", "unexpected reply card kind: " .. tostring(state.card.kind))

  -- Finishing is local lifecycle work: it must close immediately without
  -- entering Thinking or rendering a redundant Stopped card.
  loopbiotic.stop()
  assert(state.session_id == nil, "stop left the session active")
  assert(state.card == nil, "stop rendered a receipt card")
  assert(not state.thinking_request_id, "stop entered Thinking")

  -- A new session after stop also proves the daemon processed session/stop
  -- without shutting down the reusable backend process.
  loopbiotic.submit_prompt("Smoke cancellation after local stop", "investigate")
  local restarted = vim.wait(15000, function()
    return state.session_id ~= nil and state.card ~= nil and state.card.kind ~= "working"
  end, 50)
  assert(restarted, "smoke round-trip timed out after local stop")

  -- Stopping the backend mid-request must fail the in-flight callback and
  -- clear the thinking spinner instead of leaving it stuck.
  loopbiotic.submit_reply("Explain the evidence", "explain", {})
  assert(state.thinking_request_id, "expected a thinking spinner during the agent turn")
  rpc.stop()
  assert(not state.thinking_request_id, "rpc.stop left the thinking spinner active")

  rpc.stop()
  loopbiotic.reset()
  print("Loopbiotic smoke round-trip passed")
end
