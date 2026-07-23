-- The mid-turn location-permission gate: an agent request to open another
-- workspace file is presented in AgentWindow with explicit Accept / Deny and
-- is never granted silently. Every escape route (deny, stop, superseding
-- card) must answer the pending backend request exactly once.
return function(t)
  local card = require("loopbiotic.card")
  local config = require("loopbiotic.config")
  local permission = require("loopbiotic.permission")
  local state = require("loopbiotic.state")
  local surfaces = require("loopbiotic.surfaces")

  local function cleanup()
    permission.settle("test cleanup")
    require("loopbiotic.thinking").stop(false)
    surfaces.close_all()
    vim.cmd("silent! only")
    state.reset()
  end

  local function workspace_temp(suffix)
    return vim.fn.getcwd() .. "/.loopbiotic-test-" .. tostring((vim.uv or vim.loop).hrtime()) .. suffix
  end

  local function responder()
    local seen = {}
    return seen, function(result)
      table.insert(seen, result)
    end
  end

  local function request(file, reason)
    local seen, respond = responder()
    permission.request({
      session_id = "s_permission",
      reason = reason or "The change belongs in another file.",
      location = { file = file, line = 1, column = 1 },
    }, respond)
    return seen
  end

  local function delete_file(file)
    local buf = vim.fn.bufnr(vim.fn.fnamemodify(file, ":p"))
    if buf >= 0 and vim.api.nvim_buf_is_valid(buf) then
      vim.api.nvim_buf_delete(buf, { force = true })
    end
    vim.fn.delete(file)
  end

  t.test("permission request outside the workspace is denied without asking", function()
    cleanup()
    local seen = request("/etc/hosts")

    t.eq(seen, { { granted = false } }, "denied immediately")
    t.eq(state.permission, nil, "no pending gate")
    cleanup()
  end)

  t.test("workspace request renders the gate and waits instead of auto-granting", function()
    cleanup()
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;" }, file)

    local seen = request(file, "The next hunk belongs in this file.")

    t.eq(#seen, 0, "no answer before the user decides")
    t.eq(state.permission ~= nil, true, "gate is pending")
    t.eq(surfaces.agent_view(), "permission", "AgentWindow shows the permission View")
    t.eq(surfaces.snapshot().agent.working, true, "the turn still counts as active")
    local mapping = vim.fn.maparg(config.values.keymaps.draft_accept, "n", false, true)
    t.eq(type(mapping) == "table" and mapping.lhs ~= nil, true, "accept key bound globally")

    permission.settle("test teardown")
    t.eq(seen, { { granted = false } }, "settle answers the request")
    delete_file(file)
    cleanup()
  end)

  t.test("accept opens the location in the user's split and grants fresh context", function()
    cleanup()
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;", "export const other = 1;" }, file)

    -- Two splits with distinct buffers; the user works in the second split.
    local first_win = vim.api.nvim_get_current_win()
    local first_buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(first_win, first_buf)
    vim.cmd("vsplit")
    local user_win = vim.api.nvim_get_current_win()
    local user_buf = vim.api.nvim_create_buf(false, true)
    vim.api.nvim_win_set_buf(user_win, user_buf)

    local seen = request(file)
    permission.accept()

    t.eq(#seen, 1, "answered exactly once")
    t.eq(seen[1].granted, true, "granted")
    t.eq(type(seen[1].context), "table", "fresh context attached")
    t.eq(seen[1].context.file, vim.fn.fnamemodify(file, ":."), "context captures the requested file")
    t.eq(vim.api.nvim_get_current_win(), user_win, "opened in the split the user was working in")
    t.eq(
      vim.fn.fnamemodify(vim.api.nvim_buf_get_name(vim.api.nvim_win_get_buf(user_win)), ":p"),
      vim.fn.fnamemodify(file, ":p"),
      "user split shows the requested file"
    )
    t.eq(vim.api.nvim_win_get_buf(first_win), first_buf, "the other split is untouched")
    t.eq(state.permission, nil, "gate resolved")
    t.eq(vim.fn.maparg(config.values.keymaps.draft_accept, "n"), "", "global accept key removed")

    delete_file(file)
    cleanup()
  end)

  t.test("accept grants a missing workspace file as new-file context without navigating", function()
    cleanup()
    local file = workspace_temp(".ts")
    local origin_win = vim.api.nvim_get_current_win()
    local origin_buf = vim.api.nvim_win_get_buf(origin_win)

    local seen = request(file)
    permission.accept()

    t.eq(#seen, 1, "answered exactly once")
    t.eq(seen[1].granted, true, "granted")
    t.eq(seen[1].context.buffer_text, "", "empty new-file context")
    t.eq(vim.api.nvim_get_current_win(), origin_win, "no navigation happened")
    t.eq(vim.api.nvim_win_get_buf(origin_win), origin_buf, "buffer unchanged")
    cleanup()
  end)

  t.test("deny answers the request without opening anything", function()
    cleanup()
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;" }, file)
    local origin_buf = vim.api.nvim_get_current_buf()

    local seen = request(file)
    permission.deny()

    t.eq(seen, { { granted = false } }, "denied")
    t.eq(vim.api.nvim_get_current_buf(), origin_buf, "nothing opened")
    t.eq(state.permission, nil, "gate resolved")

    permission.accept()
    permission.deny()
    t.eq(#seen, 1, "late keys cannot answer twice")

    delete_file(file)
    cleanup()
  end)

  t.test("a yielded working card does not repaint over the pending gate", function()
    cleanup()
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;" }, file)
    state.session_id = "s_permission"

    local seen = request(file)
    local working = { id = "c_working_1", kind = "working", turn_id = "t_1", message = "still working" }
    card.show(working)

    t.eq(surfaces.agent_view(), "permission", "gate stays visible")
    t.eq(state.card, working, "working card recorded for agent/turn_ready")
    t.eq(#seen, 0, "gate still pending")

    -- A real turn result (e.g. the backend-side wait expired into a deny
    -- card) supersedes the gate; the late answer is stale on the daemon.
    card.show({ id = "d_1", kind = "deny", title = "Agent needs another file", reason = "expired" })
    t.eq(seen, { { granted = false } }, "superseded gate settles as denied")
    t.eq(state.permission, nil, "gate resolved")

    delete_file(file)
    cleanup()
  end)

  t.test("stop answers the pending gate before session/stop reaches the daemon", function()
    cleanup()
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;" }, file)
    state.session_id = "s_permission"

    local seen = request(file)

    local rpc = require("loopbiotic.rpc")
    local original_request = rpc.request
    local answered_before_stop
    rpc.request = function(method)
      if method == "session/stop" then
        answered_before_stop = #seen == 1
      end
    end
    local ok, err = pcall(require("loopbiotic").stop)
    rpc.request = original_request
    if not ok then
      error(err, 0)
    end

    t.eq(seen, { { granted = false } }, "pending gate denied on stop")
    t.eq(answered_before_stop, true, "answered before session/stop was sent")

    delete_file(file)
    cleanup()
  end)

  t.test("prompt interruption answers the pending gate before cancel_turn", function()
    cleanup()
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;" }, file)
    state.session_id = "s_permission"

    local seen = request(file)

    local rpc = require("loopbiotic.rpc")
    local original_request = rpc.request
    local answered_before_cancel
    rpc.request = function(method)
      if method == "session/action" then
        answered_before_cancel = #seen == 1
      end
    end
    local ok, err = pcall(require("loopbiotic").interrupt_for_prompt)
    rpc.request = original_request
    if not ok then
      error(err, 0)
    end

    t.eq(seen, { { granted = false } }, "pending gate denied on interrupt")
    t.eq(answered_before_cancel, true, "answered before cancel_turn was sent")

    delete_file(file)
    cleanup()
  end)

  t.test("a pre-existing user mapping on the review key is restored", function()
    cleanup()
    local file = workspace_temp(".ts")
    vim.fn.writefile({ "export const answer = 42;" }, file)
    local key = config.values.keymaps.draft_accept
    vim.keymap.set("n", key, "<Nop>", { desc = "user mapping" })

    request(file)
    permission.deny()

    local mapping = vim.fn.maparg(key, "n", false, true)
    t.eq(type(mapping) == "table" and mapping.desc == "user mapping", true, "user mapping restored")
    vim.keymap.del("n", key)
    delete_file(file)
    cleanup()
  end)
end
