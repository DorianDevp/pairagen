local M = {}

function M.path()
  return vim.fn.stdpath("state") .. "/pairagen.log"
end

function M.write(message, data)
  local path = M.path()
  local dir = vim.fn.fnamemodify(path, ":h")

  vim.fn.mkdir(dir, "p")

  local line = {
    os.date("%Y-%m-%dT%H:%M:%S%z"),
    message,
  }

  if data then
    table.insert(line, vim.inspect(data))
  end

  vim.fn.writefile({ table.concat(line, " ") }, path, "a")
end

return M
