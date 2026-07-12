local config = require("pair.config")

local M = {
  installing = false,
  callbacks = {},
}

function M.target()
  local uname = (vim.uv or vim.loop).os_uname()
  local system = tostring(uname.sysname or ""):lower()
  local machine = tostring(uname.machine or ""):lower()
  local architecture = ({
    x86_64 = "x86_64",
    amd64 = "x86_64",
    aarch64 = "aarch64",
    arm64 = "aarch64",
  })[machine]

  if not architecture then
    return nil, "unsupported architecture: " .. machine
  end
  if system == "linux" then
    return architecture .. "-unknown-linux-musl"
  end
  if system == "darwin" then
    return architecture .. "-apple-darwin"
  end
  return nil, "unsupported operating system: " .. system
end

function M.version()
  return (config.values.distribution and config.values.distribution.version)
    or require("pair.version").plugin
end

function M.artifact(target)
  return string.format("paird-v%s-%s.tar.gz", M.version(), target)
end

function M.install_dir(target)
  return string.format("%s/pairagen/bin/v%s/%s", vim.fn.stdpath("data"), M.version(), target)
end

function M.install_path(target)
  return M.install_dir(target) .. "/paird"
end

function M.executable(path)
  return path and vim.fn.executable(path) == 1
end

function M.resolve(callback)
  local explicit = config.values.backend.command
  if explicit and explicit ~= "" then
    callback(explicit)
    return
  end

  local target, target_error = M.target()
  if not target then
    callback(nil, target_error)
    return
  end
  local installed = M.install_path(target)
  if M.executable(installed) then
    callback(installed)
    return
  end

  local distribution = config.values.distribution or {}
  if distribution.auto_install == false then
    if vim.fn.executable("paird") == 1 then
      callback("paird")
    else
      callback(nil, "paird is not installed and automatic installation is disabled")
    end
    return
  end
  if (not distribution.repository or distribution.repository == "")
    and (not distribution.base_url or distribution.base_url == "")
  then
    if vim.fn.executable("paird") == 1 then
      callback("paird")
    else
      callback(nil, "set distribution.repository to DorianDevp/pairagen owner/repositoryor configure backend.command")
    end
    return
  end

  table.insert(M.callbacks, callback)
  if M.installing then
    return
  end
  M.installing = true
  vim.notify("Installing paird v" .. M.version(), vim.log.levels.INFO, { title = "Pair" })
  M.download(target, function(path, error_message)
    M.installing = false
    local callbacks = M.callbacks
    M.callbacks = {}
    for _, pending in ipairs(callbacks) do
      pending(path, error_message)
    end
  end)
end

function M.download(target, callback)
  if vim.fn.executable("curl") ~= 1 then
    callback(nil, "curl is required to install paird")
    return
  end
  if vim.fn.executable("tar") ~= 1 then
    callback(nil, "tar is required to install paird")
    return
  end

  local distribution = config.values.distribution
  local tag = "v" .. M.version()
  local base = distribution.base_url and distribution.base_url:gsub("/$", "") .. "/" .. tag
    or string.format("https://github.com/%s/releases/download/%s", distribution.repository, tag)
  local artifact = M.artifact(target)
  local temporary = string.format("%s/pairagen-download-%s", vim.fn.stdpath("cache"), vim.fn.getpid())
  local archive = temporary .. "/" .. artifact
  local checksums = temporary .. "/checksums.txt"
  vim.fn.delete(temporary, "rf")
  vim.fn.mkdir(temporary, "p")

  M.run({ "curl", "-fL", "--retry", "2", "-o", checksums, base .. "/checksums.txt" }, function(first)
    if first.code ~= 0 then
      callback(nil, "could not download paird checksums: " .. M.error_text(first))
      return
    end
    M.run({ "curl", "-fL", "--retry", "2", "-o", archive, base .. "/" .. artifact }, function(second)
      if second.code ~= 0 then
        callback(nil, "could not download paird: " .. M.error_text(second))
        return
      end
      local expected = M.expected_checksum(checksums, artifact)
      if not expected then
        callback(nil, "release checksums do not contain " .. artifact)
        return
      end
      M.checksum(archive, function(actual, checksum_error)
        if not actual then
          callback(nil, checksum_error)
          return
        end
        if actual:lower() ~= expected:lower() then
          callback(nil, "paird checksum mismatch")
          return
        end
        local directory = M.install_dir(target)
        vim.fn.mkdir(directory, "p")
        M.run({ "tar", "-xzf", archive, "-C", directory }, function(extracted)
          vim.fn.delete(temporary, "rf")
          if extracted.code ~= 0 then
            callback(nil, "could not extract paird: " .. M.error_text(extracted))
            return
          end
          local path = M.install_path(target)
          local uv = vim.uv or vim.loop
          uv.fs_chmod(path, 493)
          if not M.executable(path) then
            callback(nil, "installed paird is not executable: " .. path)
            return
          end
          callback(path)
        end)
      end)
    end)
  end)
end

function M.expected_checksum(path, artifact)
  if vim.fn.filereadable(path) ~= 1 then
    return nil
  end
  for _, line in ipairs(vim.fn.readfile(path)) do
    local checksum, filename = line:match("^(%x+)%s+[*]?(.+)$")
    if filename == artifact then
      return checksum
    end
  end
  return nil
end

function M.checksum(path, callback)
  local command
  if vim.fn.executable("sha256sum") == 1 then
    command = { "sha256sum", path }
  elseif vim.fn.executable("shasum") == 1 then
    command = { "shasum", "-a", "256", path }
  else
    callback(nil, "sha256sum or shasum is required to verify paird")
    return
  end
  M.run(command, function(result)
    if result.code ~= 0 then
      callback(nil, "could not verify paird: " .. M.error_text(result))
      return
    end
    callback((result.stdout or ""):match("^(%x+)"))
  end)
end

function M.run(command, callback)
  if vim.system then
    vim.system(command, { text = true }, function(result)
      vim.schedule(function()
        callback(result)
      end)
    end)
    return
  end

  local stdout = {}
  local stderr = {}
  local job = vim.fn.jobstart(command, {
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = function(_, data)
      vim.list_extend(stdout, data or {})
    end,
    on_stderr = function(_, data)
      vim.list_extend(stderr, data or {})
    end,
    on_exit = function(_, code)
      vim.schedule(function()
        callback({ code = code, stdout = table.concat(stdout, "\n"), stderr = table.concat(stderr, "\n") })
      end)
    end,
  })
  if job <= 0 then
    callback({ code = 1, stderr = "could not start " .. command[1] })
  end
end

function M.error_text(result)
  local text = vim.trim(result.stderr or "")
  return text ~= "" and text or ("exit code " .. tostring(result.code))
end

return M
