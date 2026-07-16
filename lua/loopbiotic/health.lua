local M = {}

local health = vim.health or {}
local start = health.start or health.report_start
local ok = health.ok or health.report_ok
local warn = health.warn or health.report_warn
local error = health.error or health.report_error
local info = health.info or health.report_info

function M.check()
  local config = require("loopbiotic.config")
  local installer = require("loopbiotic.installer")
  local version = require("loopbiotic.version")

  start("Loopbiotic")
  local current = vim.version()
  if current.major > 0 or current.minor >= 10 then
    ok(string.format("Neovim %d.%d.%d", current.major, current.minor, current.patch))
  else
    error("Neovim 0.10 or newer is required")
  end
  info("Plugin version: " .. version.plugin)
  info("Protocol version: " .. version.protocol)

  local target, target_error = installer.target()
  if target then
    ok("Release target: " .. target)
  else
    error(target_error)
  end

  local explicit = config.values.backend.command
  if explicit and explicit ~= "" then
    if vim.fn.executable(explicit) == 1 then
      ok("loopbioticd command: " .. explicit)
    else
      error("Configured loopbioticd command is not executable: " .. explicit)
    end
  elseif target and installer.executable(installer.install_path(target)) then
    ok("Managed loopbioticd: " .. installer.install_path(target))
  elseif config.values.distribution.auto_install == false then
    warn("Managed loopbioticd is not installed and automatic installation is disabled")
  elseif not config.values.distribution.repository and not config.values.distribution.base_url then
    error("distribution.repository is not configured")
  else
    local source = config.values.distribution.base_url or config.values.distribution.repository
    info("loopbioticd will be installed from " .. source .. " on first use")
  end

  for _, command in ipairs({ "curl", "tar" }) do
    if vim.fn.executable(command) == 1 then
      ok(command .. " is available")
    else
      error(command .. " is required for automatic loopbioticd installation")
    end
  end
  if vim.fn.executable("sha256sum") == 1 or vim.fn.executable("shasum") == 1 then
    ok("SHA-256 verification is available")
  else
    error("sha256sum or shasum is required for automatic loopbioticd installation")
  end

  local agent_name, agent = config.agent_config()
  info("Active agent: " .. agent_name)
  info("Active model: " .. require("loopbiotic").model_display())
  if agent.command then
    if vim.fn.executable(agent.command) == 1 then
      ok("Agent command: " .. agent.command)
    else
      warn("Agent command is not executable: " .. agent.command)
    end
  end

  if config.values.logging.enabled == false then
    info("Session logging is disabled")
  elseif config.values.logging.include_content == true then
    warn("Session logs include source code and prompt content")
  else
    ok("Session log content is redacted")
  end

  local clients = vim.lsp.get_clients and vim.lsp.get_clients() or vim.lsp.get_active_clients()
  if #clients == 0 then
    info("No active LSP clients")
  else
    local names = {}
    for _, client in ipairs(clients) do
      table.insert(names, client.name)
    end
    ok("Active LSP clients: " .. table.concat(names, ", "))
  end
end

return M
