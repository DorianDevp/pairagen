-- Minimal dependency-free test runner:
--   nvim --headless -u NONE -i NONE -l tests/lua/run.lua
-- Prints one line per test and exits nonzero when any test fails.

local script = debug.getinfo(1, "S").source:sub(2)
local root = vim.fn.fnamemodify(script, ":p:h:h:h")
vim.opt.runtimepath:append(root)

-- Keep test runs from writing session logs into stdpath("state").
require("loopbiotic.config").values.logging.enabled = false

local passed = 0
local failed = 0

local t = {}

---@param name string
---@param fn fun()
function t.test(name, fn)
  local ok, err = pcall(fn)
  if ok then
    passed = passed + 1
    print("ok   " .. name)
  else
    failed = failed + 1
    print("FAIL " .. name)
    print("     " .. tostring(err))
  end
end

function t.eq(actual, expected, label)
  if not vim.deep_equal(actual, expected) then
    error(
      string.format(
        "%sexpected %s, got %s",
        label and (label .. ": ") or "",
        vim.inspect(expected),
        vim.inspect(actual)
      ),
      2
    )
  end
end

-- Assert that fn raises an error whose message contains fragment.
function t.fails(fragment, fn)
  local ok, err = pcall(fn)
  if ok then
    error(string.format("expected an error containing %q, but no error was raised", fragment), 2)
  end
  if not tostring(err):find(fragment, 1, true) then
    error(string.format("expected error containing %q, got %q", fragment, tostring(err)), 2)
  end
end

local files = vim.fn.globpath(root .. "/tests/lua", "test_*.lua", false, true)
table.sort(files)
for _, file in ipairs(files) do
  local case = dofile(file)
  case(t)
end

print(string.format("%d passed, %d failed", passed, failed))
os.exit(failed == 0 and 0 or 1)
