-- Agent-proposed file operations (moves/renames): netrw-driven capture,
-- validation against the live filesystem, transactional apply, and the
-- Accept/Reject review gate shared with patches.

return function(t)
  local card = require("loopbiotic.card")
  local context = require("loopbiotic.context")
  local fileops = require("loopbiotic.fileops")
  local state = require("loopbiotic.state")

  local function fixture()
    local root = vim.fn.tempname()
    vim.fn.mkdir(root .. "/src", "p")
    vim.fn.writefile({ "export const list = 1" }, root .. "/src/invoice-list.ts")
    vim.fn.writefile({ "test" }, root .. "/src/invoice-list.spec.ts")
    return root
  end

  local function with_workspace(callback)
    local root = fixture()
    local previous = vim.fn.getcwd()
    vim.cmd("cd " .. vim.fn.fnameescape(root))
    local ok, err = pcall(callback, root)
    vim.cmd("cd " .. vim.fn.fnameescape(previous))
    vim.fn.delete(root, "rf")
    assert(ok, err)
  end

  t.test("inspect validates moves against the live filesystem", function()
    with_workspace(function()
      local plan, reason = fileops.inspect({
        { kind = "move", from = "src/invoice-list.ts", to = "src/invoice-list/invoice-list.ts" },
      })
      t.eq(reason, nil)
      t.eq(#plan.ops, 1)
      t.eq(plan.ops[1].id, "fileop-1")
      t.eq(plan.ops[1].relative_to, "src/invoice-list/invoice-list.ts")
      t.eq(vim.fn.fnamemodify(plan.missing_directories[1], ":."), "src/invoice-list")

      local _, missing = fileops.inspect({ { from = "src/missing.ts", to = "src/x.ts" } })
      t.eq(missing, "Move source does not exist: src/missing.ts")
      local _, escape = fileops.inspect({ { from = "src/invoice-list.ts", to = "../outside.ts" } })
      t.eq(escape, "File operation escapes the workspace")
      local _, exists = fileops.inspect({ { from = "src/invoice-list.ts", to = "src/invoice-list.spec.ts" } })
      t.eq(exists, "Move target already exists: src/invoice-list.spec.ts")
      local _, nested = fileops.inspect({ { from = "src", to = "src/inner" } })
      t.eq(nested, "Cannot move src into itself")
      local _, overlap = fileops.inspect({
        { from = "src/invoice-list.ts", to = "src/lib/invoice-list.ts" },
        { from = "src/invoice-list.spec.ts", to = "src/lib/invoice-list.ts" },
      })
      t.eq(overlap, "Duplicate paths across file operations")
    end)
  end)

  t.test("inspect refuses to move a path owned by an unsaved buffer", function()
    with_workspace(function(root)
      local buf = vim.fn.bufadd(root .. "/src/invoice-list.ts")
      vim.fn.bufload(buf)
      vim.bo[buf].modified = true

      local _, reason = fileops.inspect({
        { from = "src/invoice-list.ts", to = "src/invoice-list/invoice-list.ts" },
      })
      t.eq(reason, "An unsaved buffer owns src/invoice-list.ts")

      vim.bo[buf].modified = false
      pcall(vim.api.nvim_buf_delete, buf, { force = true })
    end)
  end)

  t.test("commit moves files transactionally and rejects review drift", function()
    with_workspace(function(root)
      local plan = fileops.inspect({
        { from = "src/invoice-list.ts", to = "src/invoice-list/invoice-list.ts" },
        { from = "src/invoice-list.spec.ts", to = "src/invoice-list/invoice-list.spec.ts" },
      })
      local fresh, reason = fileops.commit(plan)
      t.eq(reason, nil)
      t.eq(#fresh.ops, 2)
      t.eq(vim.fn.filereadable(root .. "/src/invoice-list/invoice-list.ts"), 1)
      t.eq(vim.fn.filereadable(root .. "/src/invoice-list/invoice-list.spec.ts"), 1)
      t.eq(vim.fn.filereadable(root .. "/src/invoice-list.ts"), 0)

      -- The filesystem no longer matches the reviewed plan: nothing moves.
      local again, drift = fileops.commit(plan)
      t.eq(again, nil)
      t.eq(type(drift), "string")
    end)
  end)

  t.test("accepted moves retarget loaded buffers to the new path", function()
    with_workspace(function(root)
      local buf = vim.fn.bufadd(root .. "/src/invoice-list.ts")
      vim.fn.bufload(buf)
      local win = vim.api.nvim_get_current_win()
      vim.api.nvim_win_set_buf(win, buf)

      local plan = fileops.inspect({
        { from = "src/invoice-list.ts", to = "src/invoice-list/invoice-list.ts" },
      })
      t.eq(fileops.commit(plan) ~= nil, true)

      local shown = vim.api.nvim_win_get_buf(win)
      t.eq(
        vim.fn.fnamemodify(vim.api.nvim_buf_get_name(shown), ":."),
        "src/invoice-list/invoice-list.ts",
        "the window follows the moved file"
      )
      t.eq(vim.api.nvim_buf_is_valid(buf), false, "the stale buffer is removed")
      vim.cmd("enew")
    end)
  end)

  t.test("a directory buffer is a valid prompt source", function()
    with_workspace(function(root)
      vim.cmd("edit " .. vim.fn.fnameescape(root .. "/src"))
      local buf = vim.api.nvim_get_current_buf()
      -- Netrw marks its listing buffers nofile; the directory name is what
      -- makes it a legitimate source.
      vim.bo[buf].buftype = "nofile"
      t.eq(context.directory_source(buf), true)

      local source = context.capture(nil, { skip_lsp = true })
      t.eq(source.buf, buf, "capture keeps the directory listing as the source")
      t.eq(source.value.file, "src")
      vim.cmd("enew")
    end)
  end)

  t.test("a patch card with file_ops reviews behind Accept/Reject", function()
    with_workspace(function()
      state.reset()
      state.session_id = "s_test"
      state.goal = {
        statement = "Move these files to invoice-list",
        completed_steps = {},
        known_observations = {},
        status = "active",
      }
      local ops_card = {
        id = "c_ops",
        kind = "patch",
        title = "Move into invoice-list",
        explanation = "Group the invoice list module before fixing imports.",
        patches = {},
        file_ops = {
          { id = "fo_1", kind = "move", from = "src/invoice-list.ts", to = "src/invoice-list/invoice-list.ts" },
        },
      }

      card.show(ops_card, { enter = false })
      t.eq(fileops.pending(), true, "the validated plan is pending")
      t.eq(require("loopbiotic.surfaces").agent_view(), "review")
      t.eq(require("loopbiotic.scope").allows("accept"), true)
      t.eq(require("loopbiotic.scope").allows("prompt"), false, "an unresolved review blocks the prompt route")

      fileops.clear()
      t.eq(state.file_ops, nil)
      t.eq(require("loopbiotic.scope").allows("accept"), false)

      require("loopbiotic.surfaces").close_all()
      state.reset()
      vim.cmd("silent only")
    end)
  end)

  t.test("a mixed proposal is rejected as inert content", function()
    with_workspace(function()
      state.reset()
      local mixed = {
        id = "c_mixed",
        kind = "patch",
        title = "Mixed",
        explanation = "E",
        patches = { { id = "p_1", file = "src/invoice-list.ts", diff = "@@ -1,1 +1,1 @@\n-a\n+b\n" } },
        file_ops = { { id = "fo_1", kind = "move", from = "src/invoice-list.ts", to = "src/x.ts" } },
      }
      local original_notify = vim.notify
      vim.notify = function() end
      local shown = fileops.show(mixed, {})
      vim.notify = original_notify
      t.eq(shown or false, false)
      t.eq(fileops.pending(), false)
    end)
  end)
end
