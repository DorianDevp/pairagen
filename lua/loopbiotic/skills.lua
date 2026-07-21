local config = require("loopbiotic.config")
local state = require("loopbiotic.state")
local surfaces = require("loopbiotic.surfaces")
local ui = require("loopbiotic.ui")
local util = require("loopbiotic.util")

local M = {}

local function relative_path(root, path)
  local relative = util.relative_path(root, path)
  if not relative or relative == "" or relative:find("^%.%./") then
    return nil
  end
  return relative
end

local function safe_markdown(root, path)
  local normalized = vim.fs.normalize(path)
  local relative = relative_path(root, normalized)
  local real_root = vim.uv.fs_realpath(root)
  local real_path = vim.uv.fs_realpath(normalized)
  if
    not relative
    or not relative:lower():match("%.md$")
    or not real_root
    or not real_path
    or not util.in_workspace(real_path, real_root)
  then
    return nil
  end
  local stat = vim.uv.fs_stat(normalized)
  local limit = tonumber((config.values.skills or {}).max_file_bytes) or 65536
  if not stat or stat.type ~= "file" or stat.size > limit then
    return nil
  end
  return relative
end

local function configured_paths(root)
  local paths = {}
  for _, path in ipairs((config.values.skills or {}).autoload or {}) do
    if type(path) == "string" and path ~= "" then
      table.insert(paths, vim.fs.normalize(root .. "/" .. path))
    end
  end
  return paths
end

function M.discover(root)
  root = vim.fs.normalize(root or vim.fn.getcwd())
  local auto = {}
  for _, path in ipairs(configured_paths(root)) do
    auto[path] = true
  end
  local paths = {}
  if (config.values.skills or {}).discover_root_markdown ~= false then
    paths = vim.fn.globpath(root, "*.md", false, true)
  end
  for path in pairs(auto) do
    table.insert(paths, path)
  end

  local seen = {}
  local items = {}
  for _, path in ipairs(paths) do
    path = vim.fs.normalize(path)
    local relative = safe_markdown(root, path)
    if relative and not seen[relative] then
      seen[relative] = true
      table.insert(items, {
        name = vim.fs.basename(relative),
        path = relative,
        absolute_path = path,
        provenance = auto[path] and "config" or "workspace_root",
        auto = auto[path] == true,
      })
    end
  end
  table.sort(items, function(left, right)
    if left.auto ~= right.auto then
      return left.auto
    end
    return left.path < right.path
  end)
  return items
end

function M.prepare(root)
  root = vim.fs.normalize(root or vim.fn.getcwd())
  if state.skills_root and state.skills_root ~= root and state.session_id then
    return
  end
  if state.skills_root ~= root then
    state.selected_instruction_skills = {}
  end
  state.skills_root = root
  state.instruction_skill_catalog = M.discover(root)
  for _, item in ipairs(state.instruction_skill_catalog) do
    if item.auto then
      state.selected_instruction_skills[item.path] = true
    end
  end
end

function M.items()
  return vim.deepcopy(state.instruction_skill_catalog or {})
end

function M.selected(path)
  return state.selected_instruction_skills[path] == true
end

function M.toggle(path)
  for _, item in ipairs(state.instruction_skill_catalog or {}) do
    if item.path == path then
      if item.auto then
        return false
      end
      state.selected_instruction_skills[path] = not M.selected(path)
      return true
    end
  end
  return false
end

function M.summary()
  local names = {}
  for _, item in ipairs(state.instruction_skill_catalog or {}) do
    if M.selected(item.path) then
      table.insert(names, item.name)
    end
  end
  if #names == 0 then
    return nil
  end
  if #names > 2 then
    return string.format("Skills %s · %s +%d", names[1], names[2], #names - 2)
  end
  return "Skills " .. table.concat(names, " · ")
end

local function read_skill(item)
  if safe_markdown(state.skills_root, item.absolute_path) ~= item.path then
    return nil
  end
  local ok, lines = pcall(vim.fn.readfile, item.absolute_path)
  if not ok then
    return nil
  end
  local content = table.concat(lines, "\n")
  return {
    name = item.name,
    path = item.path,
    content = content,
    provenance = item.provenance,
    auto = item.auto,
    sha256 = vim.fn.sha256(content),
  }
end

function M.snapshot()
  local skills = {}
  for _, item in ipairs(state.instruction_skill_catalog or {}) do
    if M.selected(item.path) then
      local skill = read_skill(item)
      if skill then
        table.insert(skills, skill)
      end
    end
  end
  return skills
end

function M.activate(skills)
  state.selected_instruction_skills = {}
  for _, skill in ipairs(skills or {}) do
    state.selected_instruction_skills[skill.path] = true
  end
end

function M.attach(params, skills)
  if not state.skills_root and params.cwd then
    M.prepare(params.cwd)
  end
  params.skills = vim.deepcopy(skills or M.snapshot())
  require("loopbiotic.log").event("instruction_skills", params.skills)
  return params
end

local function picker_lines(items)
  local lines = {}
  for _, item in ipairs(items) do
    local marker = item.auto and "a" or (M.selected(item.path) and "x" or " ")
    local suffix = item.auto and "  auto" or ""
    table.insert(lines, string.format("[%s] %s%s", marker, item.path, suffix))
  end
  return lines
end

function M.open_picker(opts)
  opts = opts or {}
  if not surfaces.prompt_open() then
    return
  end
  local items = state.instruction_skill_catalog or {}
  if #items == 0 then
    ui.notify("No Markdown skills found in the workspace root", vim.log.levels.INFO)
    return
  end
  local before = vim.deepcopy(state.selected_instruction_skills)
  local buf, win = surfaces.open_prompt_picker(picker_lines(items), {
    title = " Loopbiotic Skills · session ",
    footer = " Space toggle  Enter apply  Esc cancel ",
    filetype = "loopbiotic-skills",
  })
  if not buf or not win then
    return
  end
  vim.bo[buf].modifiable = false
  vim.wo[win].cursorline = true
  local function redraw()
    vim.bo[buf].modifiable = true
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, picker_lines(items))
    vim.bo[buf].modifiable = false
  end
  local function return_to_prompt()
    surfaces.close_prompt_picker({ focus_prompt = true })
    require("loopbiotic.prompt").refresh_footer()
    if opts.return_to_insert then
      vim.schedule(function()
        if surfaces.prompt_open() then
          vim.cmd("startinsert")
        end
      end)
    end
  end
  vim.keymap.set("n", "<Space>", function()
    local row = vim.api.nvim_win_get_cursor(win)[1]
    if items[row] and M.toggle(items[row].path) then
      redraw()
    end
  end, { buffer = buf, nowait = true, silent = true })
  vim.keymap.set("n", "<CR>", function()
    return_to_prompt()
  end, { buffer = buf, nowait = true, silent = true })
  local function cancel()
    state.selected_instruction_skills = before
    return_to_prompt()
  end
  for _, lhs in ipairs({ "q", "<Esc>" }) do
    vim.keymap.set("n", lhs, cancel, { buffer = buf, nowait = true, silent = true })
  end
end

function M.reset()
  state.skills_root = nil
  state.instruction_skill_catalog = {}
  state.selected_instruction_skills = {}
end

return M
