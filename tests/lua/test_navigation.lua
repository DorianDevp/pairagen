-- Cursor targets can point past the end of the real buffer: card locations
-- come from the agent, and draft cursors are computed against post-apply
-- content (e.g. a hunk appending to a one-line barrel index.ts).
local util = require("loopbiotic.util")
local navigation = require("loopbiotic.navigation")

return function(t)
  local function workspace_temp(suffix)
    return vim.fn.getcwd() .. "/.loopbiotic-test-" .. tostring((vim.uv or vim.loop).hrtime()) .. suffix
  end

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
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export * from './lib/ui-icon-header/ui-icon-header.component';" }, file)

    -- Line 2 of a one-line file: the first added line of an appending draft.
    local ok = navigation.open_location({ file = file, line = 2, column = 1 })

    t.eq(ok, true)
    t.eq(vim.api.nvim_win_get_cursor(0)[1], 1)

    vim.cmd("bwipeout!")
    vim.fn.delete(file)
  end)

  t.test("tab navigation leaves the origin tab on a normal window", function()
    local config = require("loopbiotic.config")
    local surfaces = require("loopbiotic.surfaces")
    local previous_open = config.values.navigation.open
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;" }, file)

    local origin_tab = vim.api.nvim_get_current_tabpage()
    local origin_win = vim.api.nvim_get_current_win()
    local float_buf, float_win = surfaces.render_agent({ "Focused AgentWindow" }, {
      view = "response",
      enter = true,
      window = { width = 32, height = 1 },
    })
    config.values.navigation.open = "tab"

    local ok, err = pcall(function()
      t.eq(navigation.open_location({ file = file, line = 1, column = 1 }), true)
      t.eq(vim.api.nvim_get_current_tabpage() ~= origin_tab, true, "opened another tab")
      t.eq(vim.api.nvim_tabpage_get_win(origin_tab), origin_win, "origin current window")
      t.eq(vim.api.nvim_win_is_valid(float_win), true, "float remains valid")
    end)

    surfaces.close_agent()
    vim.api.nvim_set_current_tabpage(origin_tab)
    require("loopbiotic.ui").cleanup_deferred()
    vim.cmd("tabonly")
    if vim.api.nvim_buf_is_valid(float_buf) then
      vim.api.nvim_buf_delete(float_buf, { force = true })
    end
    local target_buf = vim.fn.bufnr(file)
    if target_buf >= 0 and vim.api.nvim_buf_is_valid(target_buf) then
      vim.api.nvim_buf_delete(target_buf, { force = true })
    end
    vim.fn.delete(file)
    config.values.navigation.open = previous_open

    if not ok then
      error(err, 0)
    end
  end)
end
