-- Cursor targets can point past the end of the real buffer: card locations
-- come from the agent, and draft cursors are computed against post-apply
-- content (e.g. a hunk appending to a one-line barrel index.ts).
local util = require("loopbiotic.util")
local navigation = require("loopbiotic.navigation")

return function(t)
  t.test("clamp_cursor keeps valid positions and floors the column", function()
    local buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, { "one", "two", "three" })

    t.eq(util.clamp_cursor(buf, 2, 1), { 2, 1 })
    t.eq(util.clamp_cursor(buf, nil, nil), { 1, 0 })
    t.eq(util.clamp_cursor(buf, 0, -5), { 1, 0 })

    vim.api.nvim_buf_delete(buf, { force = true })
  end)

  t.test("clamp_cursor bounds lines past the end of the buffer", function()
    local buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, { "export * from './lib/thing';" })

    t.eq(util.clamp_cursor(buf, 2, 0), { 1, 0 })
    t.eq(util.clamp_cursor(buf, 500, 3), { 1, 3 })

    vim.api.nvim_buf_delete(buf, { force = true })
  end)

  t.test("open_location survives a line past the end of a short file", function()
    local file = vim.fn.tempname() .. ".ts"
    vim.fn.writefile({ "export * from './lib/ui-icon-header/ui-icon-header.component';" }, file)

    -- Line 2 of a one-line file: the first added line of an appending draft.
    local ok = navigation.open_location({ file = file, line = 2, column = 1 })

    t.eq(ok, true)
    t.eq(vim.api.nvim_win_get_cursor(0)[1], 1)

    vim.cmd("bwipeout!")
    vim.fn.delete(file)
  end)
end
