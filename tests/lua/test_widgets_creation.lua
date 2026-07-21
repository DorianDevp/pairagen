return function(t)
  local creation = require("loopbiotic.creation")
  local state = require("loopbiotic.state")
  local widgets = require("loopbiotic.widgets")

  t.test("Widget envelopes reject unknown protocols and strip unregistered intents", function()
    local unsupported, reason = widgets.validate({ id = "w", kind = "shell", version = 1, data = {}, intents = {} })
    t.eq(unsupported, nil)
    t.eq(reason, "unsupported widget kind or version")

    local valid = widgets.validate({
      id = "flow",
      kind = "flow",
      version = 1,
      data = { graph = {} },
      intents = { "navigate", "execute_command", "select_context" },
    })
    t.eq(valid.intents, { "navigate", "select_context" })

    -- The per-widget validator actually runs: a flow envelope missing its
    -- graph is rejected. (Regression: an `and/or` fold used to swallow every
    -- validator's `false` result, accepting malformed payloads.)
    local bad, bad_reason = widgets.validate({
      id = "flow",
      kind = "flow",
      version = 1,
      data = {},
      intents = {},
    })
    t.eq(bad, nil)
    t.eq(bad_reason, "Flow requires an editor-resolved graph")
  end)

  t.test("Widget selection is visible, removable and attached only as prompt context", function()
    state.reset()
    local file = vim.fn.getcwd() .. "/lua/loopbiotic/widgets.lua"
    local selected = widgets.select({
      id = "flow:symbol:widgets",
      kind = "symbol",
      file = file,
      range = { start_line = 1, start_column = 1, end_line = 2, end_column = 1 },
      label = "widgets",
      provenance = "lsp",
    })
    t.eq(selected, true)
    t.eq(widgets.summary(), "Context 1 ref · 1 file")

    local context = widgets.attach({ hints = {} })
    t.eq(#context.hints, 1)
    t.eq(context.hints[1].source, "widget:lsp:flow:symbol:widgets")
    t.eq(state.pending_widget_context["flow:symbol:widgets"] ~= nil, true, "attach does not submit or clear")
    widgets.deselect("flow:symbol:widgets")
    t.eq(widgets.summary(), nil)
  end)

  t.test("Widget context cannot escape the workspace", function()
    state.reset()
    local ok, reason = widgets.select({
      id = "outside",
      kind = "file",
      file = "/tmp/outside.lua",
      label = "outside",
      provenance = "agent",
    })
    t.eq(ok, false)
    t.eq(reason, "widget context is outside the workspace")
  end)

  t.test("new-file creation revalidates collision and commits one safe set", function()
    local root = vim.fn.getcwd() .. "/.loopbiotic-test-" .. tostring((vim.uv or vim.loop).hrtime())
    local target = root .. "/nested/new.lua"
    local plan, reason = creation.inspect(target)
    t.eq(reason, nil)
    t.eq(plan.relative:find("new.lua", 1, true) ~= nil, true)
    local ok, commit_error = creation.commit(plan, { "return true" })
    t.eq(ok, true, commit_error)
    t.eq(vim.fn.readfile(target), { "return true" })
    local duplicate, duplicate_error = creation.inspect(target)
    t.eq(duplicate, nil)
    t.eq(duplicate_error, "Creation target already exists")
    vim.fn.delete(target)
    vim.fn.delete(root .. "/nested", "d")
    vim.fn.delete(root, "d")
  end)

  t.test("new-file review keeps Netrw parent context beside the inert source buffer", function()
    local diff = require("loopbiotic.diff")
    local target = vim.fn.getcwd() .. "/.loopbiotic-review-" .. tostring((vim.uv or vim.loop).hrtime()) .. "/new.lua"
    local plan = assert(creation.inspect(target))
    local source_buf = vim.fn.bufadd(target)
    vim.fn.bufload(source_buf)
    local opened = diff.open_creation_context(plan, source_buf)
    t.eq(opened, true)
    t.eq(vim.api.nvim_get_current_buf(), source_buf, "draft side stays active")
    local parent_visible = false
    for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
      local name = vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(win))
      if vim.fn.fnamemodify(name, ":p") == vim.fn.fnamemodify(plan.existing_parent, ":p") then
        parent_visible = true
      end
    end
    t.eq(parent_visible, true, "nearest existing parent is visible")
    vim.cmd("only")
    if vim.api.nvim_buf_is_valid(source_buf) then
      vim.api.nvim_buf_delete(source_buf, { force = true })
    end
  end)
end
