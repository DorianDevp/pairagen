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

local installer = require("pair.installer")
assert(installer.artifact("x86_64-unknown-linux-musl") == "paird-v0.1.0-x86_64-unknown-linux-musl.tar.gz")

local log = require("pair.log")
local sanitized = log.sanitize({ buffer_text = "secret source", event = "kept" })
assert(sanitized.buffer_text.redacted == true)
assert(sanitized.buffer_text.bytes > 0)
assert(sanitized.event == "kept")

print("Pairagen headless smoke test passed")
