-- Recovery when a queued patch no longer applies: the daemon validated the
-- patch at queue time, but the user can edit the buffer during review. A
-- stale draft must offer a way back into the goal loop (retry redrafts the
-- current slice, which is cheap), not dead-end on a raw error notification.
local diff = require("loopbiotic.diff")

return function(t)
  local retry_label = "Retry slice with current buffer"

  -- Run fn with vim.ui.select and vim.notify captured; always restores both.
  local function with_recovery_stubs(fn)
    local original_select = vim.ui.select
    local original_notify = vim.notify
    local captured = { selects = {}, notifications = {} }
    vim.ui.select = function(items, opts, on_choice)
      table.insert(captured.selects, { items = items, opts = opts, on_choice = on_choice })
    end
    vim.notify = function(message, level)
      table.insert(captured.notifications, { message = message, level = level })
    end
    local ok, err = pcall(fn, captured)
    vim.ui.select = original_select
    vim.notify = original_notify
    if not ok then
      error(err, 0)
    end
  end

  -- Open a scratch file whose buffer content has drifted to `lines` after
  -- the card was drafted, and return a queued patch card targeting it.
  local function drifted_card(lines, diff_text, actions)
    local file = vim.fn.tempname() .. ".txt"
    vim.fn.writefile(lines, file)
    vim.cmd("edit " .. vim.fn.fnameescape(file))
    vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)

    return {
      id = "card-1",
      kind = "patch",
      actions = actions,
      patches = { { id = "patch-1", file = file, diff = diff_text } },
    },
      file
  end

  local function cleanup(file)
    vim.cmd("bwipeout!")
    vim.fn.delete(file)
  end

  t.test("recovery_plan offers retry-first recovery for drift", function()
    local plan = diff.recovery_plan("drift", { "retry", "edit_prompt", "stop" })
    t.eq(plan.reason, "draft no longer matches the buffer (edited since it was drafted)")
    t.eq(#plan.choices, 2, "choice count")
    t.eq(plan.choices[1], { label = retry_label, action = "retry" }, "retry is first, the default")
    t.eq(plan.choices[2], { label = "Cancel", action = "cancel" })
  end)

  t.test("recovery_plan explains malformed drafts differently", function()
    local plan = diff.recovery_plan("malformed", { "retry" })
    t.eq(plan.reason, "the drafted patch is malformed")
    t.eq(plan.choices[1].action, "retry", "retry is the only sensible offer")
    t.eq(plan.choices[2].action, "cancel")
  end)

  t.test("recovery_plan finds retry among mixed action entries", function()
    local plan = diff.recovery_plan("drift", { { apply_patch = { id = "p1" } }, "retry" })
    t.eq(plan ~= nil, true, "table entries do not hide the retry action")
  end)

  t.test("recovery_plan offers nothing when the card cannot retry", function()
    t.eq(diff.recovery_plan("drift", { "edit_prompt", "stop" }), nil, "no retry action")
    t.eq(diff.recovery_plan("malformed", {}), nil, "empty actions")
    t.eq(diff.recovery_plan("drift", nil), nil, "missing actions")
  end)

  t.test("show prompts for recovery when the buffer drifted", function()
    local card, file = drifted_card({ "edited since the draft" }, "@@ -1,1 +1,1 @@\n-original\n+patched\n", { "retry" })

    with_recovery_stubs(function(captured)
      t.eq(diff.show(card), false, "not shown")

      t.eq(#captured.selects, 1, "recovery selector invoked")
      local select = captured.selects[1]
      t.eq(#select.items, 2, "two choices")
      t.eq(select.opts.format_item(select.items[1]), retry_label, "retry is the first choice")
      t.eq(
        select.opts.prompt:find("no longer matches the buffer", 1, true) ~= nil,
        true,
        "prompt names the drift reason"
      )

      local errors = vim.tbl_filter(function(entry)
        return entry.level == vim.log.levels.ERROR
      end, captured.notifications)
      t.eq(errors, {}, "no raw error notification")
    end)

    cleanup(file)
  end)

  t.test("choosing retry fires the retry action; cancel does not", function()
    local card, file = drifted_card({ "edited since the draft" }, "@@ -1,1 +1,1 @@\n-original\n+patched\n", { "retry" })

    with_recovery_stubs(function(captured)
      local loopbiotic = require("loopbiotic")
      local original_action = loopbiotic.action
      local actions = {}
      loopbiotic.action = function(name, opts)
        table.insert(actions, { name = name, opts = opts })
      end

      local ok, err = pcall(function()
        diff.show(card)
        local select = captured.selects[1]

        select.on_choice(nil) -- dismissed
        select.on_choice(select.items[2]) -- explicit cancel
        t.eq(actions, {}, "no agent turn without the user's pick")

        select.on_choice(select.items[1])
        t.eq(actions, { { name = "retry", opts = { allow_hidden = true } } }, "retry fired once")
      end)

      loopbiotic.action = original_action
      if not ok then
        error(err, 0)
      end
    end)

    cleanup(file)
  end)

  t.test("show prompts for recovery on a malformed queued patch", function()
    local card, file = drifted_card({ "anything" }, "@@ -1,2 +1,3 @@\n+added only\n", { "retry" })

    with_recovery_stubs(function(captured)
      t.eq(diff.show(card), false, "not shown")
      t.eq(#captured.selects, 1, "recovery selector invoked")
      t.eq(captured.selects[1].opts.prompt:find("malformed", 1, true) ~= nil, true, "prompt names the parse failure")
    end)

    cleanup(file)
  end)

  t.test("show falls back to a plain error when the card cannot retry", function()
    local card, file = drifted_card({ "edited since the draft" }, "@@ -1,1 +1,1 @@\n-original\n+patched\n", { "stop" })

    with_recovery_stubs(function(captured)
      t.eq(diff.show(card), false, "not shown")
      t.eq(#captured.selects, 0, "no selector without a retry action")
      t.eq(#captured.notifications, 1, "raw error notified")
      t.eq(captured.notifications[1].level, vim.log.levels.ERROR, "error level")
      t.eq(captured.notifications[1].message:find("not found", 1, true) ~= nil, true, "resolution failure surfaced")
    end)

    cleanup(file)
  end)
end
