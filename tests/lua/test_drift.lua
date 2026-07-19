return function(t)
  local diff = require("loopbiotic.diff")

  local function card_for(lines, patch)
    local file = vim.fn.getcwd() .. "/.loopbiotic-test-" .. tostring((vim.uv or vim.loop).hrtime()) .. ".txt"
    vim.fn.writefile(lines, file)
    vim.cmd("edit " .. vim.fn.fnameescape(file))
    return {
      id = "card",
      kind = "patch",
      patches = { { id = "patch", file = file, diff = patch } },
      actions = { "retry" },
    }, file
  end

  local function inert_error(card, expected)
    local original = vim.notify
    local notices = {}
    vim.notify = function(message, level)
      table.insert(notices, { message = message, level = level })
    end
    local ok, shown = pcall(diff.show, card)
    vim.notify = original
    if not ok then
      error(shown, 0)
    end
    t.eq(shown, false)
    t.eq(#notices, 1, "one inert error")
    t.eq(notices[1].message:find(expected, 1, true) ~= nil, true)
  end

  t.test("stale review is inert and never offers Retry", function()
    local card, file = card_for({ "changed" }, "@@ -1,1 +1,1 @@\n-old\n+new\n")
    inert_error(card, "not found")
    vim.cmd("bwipeout!")
    vim.fn.delete(file)
  end)

  t.test("malformed review is inert and never offers Retry", function()
    local card, file = card_for({ "anything" }, "@@ -1,2 +1,3 @@\n+only addition\n")
    inert_error(card, "source context")
    vim.cmd("bwipeout!")
    vim.fn.delete(file)
  end)
end
