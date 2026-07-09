local M = {}

M.values = {
  backend = {
    command = "paird",
    args = {},
    mode = "auto",
  },
  keymaps = {
    prompt = "<leader>a",
  },
  prompt = {
    border = "rounded",
  },
  card = {
    border = "rounded",
    max_width = 72,
    max_height = 12,
  },
  navigation = {
    open = "tab",
    annotate = true,
  },
  diff = {
    layout = "tab",
    apply_to_buffer = true,
  },
}

function M.setup(opts)
  M.values = vim.tbl_deep_extend("force", M.values, opts or {})

  return M.values
end

return M
