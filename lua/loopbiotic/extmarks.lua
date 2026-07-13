local config = require("loopbiotic.config")

local M = {
  ns = vim.api.nvim_create_namespace("loopbiotic"),
}

function M.annotate(buf, line, text)
  if not config.values.navigation.annotate or not text or text == "" then
    return
  end

  vim.api.nvim_buf_set_extmark(buf, M.ns, math.max(line - 1, 0), 0, {
    virt_lines = {
      {
        { "Loopbiotic: ", "Title" },
        { text, "Comment" },
      },
    },
  })
end

return M
