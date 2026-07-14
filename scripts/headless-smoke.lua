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
assert(config.values.backend.prefetch == "off")
assert(config.values.backend.token_budget == 50000)

local legacy_preferences = vim.fn.stdpath("state") .. "/pairagen/preferences.json"
vim.fn.delete(config.preferences_path())
vim.fn.mkdir(vim.fn.fnamemodify(legacy_preferences, ":h"), "p")
vim.fn.writefile({ vim.json.encode({ models = { codex = "legacy-model" } }) }, legacy_preferences)
assert(config.read_preferences().models.codex == "legacy-model")
vim.fn.delete(legacy_preferences)

local prompt = require("loopbiotic.prompt")
assert(prompt.title("Prompt") == " Loopbiotic Prompt · codex / test-model ")
prompt.open_for({ title = prompt.title("Reply"), footer = " Test ", submit = function() end })
local state = require("loopbiotic.state")
assert(vim.api.nvim_win_get_config(state.prompt_frame_win).zindex == 200)
assert(vim.api.nvim_win_get_config(state.prompt_win).zindex == 201)
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

local check_buf = vim.fn.bufadd(vim.fn.getcwd() .. "/src/check-test.lua")
vim.fn.bufload(check_buf)
local check_namespace = vim.api.nvim_create_namespace("loopbiotic-headless-check")
vim.diagnostic.set(check_namespace, check_buf, {
  { lnum = 2, col = 0, severity = vim.diagnostic.severity.ERROR, message = "broken check" },
})
local check = require("loopbiotic").editor_check({ "src/check-test.lua" })
assert(check.checked_files == 1)
assert(#check.errors == 1)
assert(check.errors[1].line == 3)
vim.diagnostic.reset(check_namespace, check_buf)
vim.api.nvim_buf_delete(check_buf, { force = true })

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
vim.cmd("enew")
local focus_source = vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_name(focus_source, root .. "/loopbiotic-focus-test.lua")
vim.api.nvim_buf_set_lines(focus_source, 0, -1, false, { "local before = true", "return before" })
local focus_card = {
  id = "focus-card",
  kind = "patch",
  title = "Focus inserted text",
  explanation = "Keep the cursor on the change",
  patches = {
    {
      id = "focus-patch",
      file = "loopbiotic-focus-test.lua",
      diff = "@@ -1,2 +1,2 @@\n local before = true\n-return before\n+  return not before\n",
    },
  },
}
state.card = focus_card
assert(diff.show(focus_card))
assert(vim.api.nvim_get_current_buf() == state.diff_buf)
assert(vim.deep_equal(vim.api.nvim_win_get_cursor(0), { 2, 2 }))
diff.restore_source()
assert(vim.api.nvim_get_current_buf() == focus_source)
state.card = nil
vim.api.nvim_buf_delete(focus_source, { force = true })
local long_goal = "Mam tutaj problem, bo ta pętla nie uwzględnia wszystkich elementów z kolejnych przebiegów"
local long_explanation = "Oznacza korzeń podglądu tym samym dyskryminatorem w węźle nadrzędnym i zachowuje pełny opis zmiany"
local patch_card = {
  kind = "patch",
  title = "Preview root",
  explanation = long_explanation,
}
require("loopbiotic.commands").setup()
assert(vim.fn.exists(":LoopbioticAssess") == 2)
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
assert(table.concat(compact, "\n"):find("Why this hunk", 1, true))
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
assert(installer.artifact("x86_64-unknown-linux-musl") == "loopbioticd-v0.3.1-x86_64-unknown-linux-musl.tar.gz")

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

local location_card = {
  id = "location-card",
  kind = "finding",
  title = "Relevant call",
  finding = "This is where the function is called.",
  location = { file = "README.md", line = 4, column = 2 },
  next_actions = { "stop" },
}
state.session_id = "headless-session"
card.show(location_card)
assert(vim.deep_equal(vim.api.nvim_win_get_cursor(0), { 4, 1 }))
local previous_float_win = state.card_win
vim.cmd("tabnew")
vim.wait(1000, function()
  return state.card_win
    and vim.api.nvim_win_is_valid(state.card_win)
    and vim.api.nvim_win_get_tabpage(state.card_win) == vim.api.nvim_get_current_tabpage()
end)
assert(vim.api.nvim_win_get_tabpage(state.card_win) == vim.api.nvim_get_current_tabpage())
assert(not vim.api.nvim_win_is_valid(previous_float_win))
require("loopbiotic.ui").close(state.card_win)
state.card_win = nil
state.card = nil
state.last_card = nil
state.session_id = nil
state.navigated_card = nil
vim.cmd("tabonly")

print("Loopbiotic headless smoke test passed")
