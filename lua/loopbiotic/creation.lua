local util = require("loopbiotic.util")

local M = {}

local function normalized(path)
  return vim.fs.normalize(vim.fn.fnamemodify(path, ":p"))
end

function M.inspect(path)
  local target = normalized(path)
  if not util.in_workspace(target) then
    return nil, "Creation target is outside the workspace"
  end
  if vim.uv.fs_stat(target) then
    return nil, "Creation target already exists"
  end

  local parent = vim.fs.dirname(target)
  local missing = {}
  while parent and not vim.uv.fs_stat(parent) do
    table.insert(missing, 1, parent)
    local next_parent = vim.fs.dirname(parent)
    if next_parent == parent then
      return nil, "Creation target has no existing parent"
    end
    parent = next_parent
  end
  local workspace = vim.uv.fs_realpath(vim.fn.getcwd())
  local real_parent = parent and vim.uv.fs_realpath(parent)
  if not workspace or not real_parent or not util.in_workspace(real_parent, workspace) then
    return nil, "Creation parent resolves outside the workspace"
  end
  return {
    target = target,
    relative = vim.fn.fnamemodify(target, ":."),
    existing_parent = real_parent,
    missing_directories = missing,
  }
end

function M.commit(plan, lines)
  local fresh, reason = M.inspect(plan.target)
  if not fresh then
    return false, reason
  end
  if fresh.existing_parent ~= plan.existing_parent then
    return false, "Creation parent changed during review"
  end

  local created = {}
  for _, directory in ipairs(plan.missing_directories) do
    if vim.fn.mkdir(directory) == 0 and vim.fn.isdirectory(directory) ~= 1 then
      for index = #created, 1, -1 do
        pcall(vim.fn.delete, created[index], "d")
      end
      return false, "Could not create directory " .. directory
    end
    table.insert(created, directory)
  end
  if vim.fn.writefile(lines, plan.target) ~= 0 then
    pcall(vim.fn.delete, plan.target)
    for index = #created, 1, -1 do
      pcall(vim.fn.delete, created[index], "d")
    end
    return false, "Could not create file " .. plan.relative
  end
  return true
end

return M
