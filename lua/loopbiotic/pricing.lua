-- Token pricing for the card's billing line.
--
-- Rates are USD per 1,000,000 tokens. `cached_input` is the prompt-cache READ
-- rate (~10x cheaper than fresh input), which is why a tool-heavy turn that
-- reads tens of thousands of cached tokens still costs very little. Override or
-- extend via `pricing` in setup(), keyed by model id.
local M = {}

M.rates = {
  ["claude-opus-4-8"] = { input = 5.0, cached_input = 0.5, output = 25.0 },
  ["claude-opus-4-7"] = { input = 5.0, cached_input = 0.5, output = 25.0 },
  ["claude-opus-4-6"] = { input = 5.0, cached_input = 0.5, output = 25.0 },
  ["claude-sonnet-5"] = { input = 3.0, cached_input = 0.3, output = 15.0 },
  ["claude-sonnet-4-6"] = { input = 3.0, cached_input = 0.3, output = 15.0 },
  ["claude-haiku-4-5"] = { input = 1.0, cached_input = 0.1, output = 5.0 },
  ["haiku"] = { input = 1.0, cached_input = 0.1, output = 5.0 },
}

-- Resolve the rate for a model id: exact match, then user overrides, then a
-- substring match so labels like "codex/gpt-5.1" or dated suffixes still hit.
function M.rate_for(model)
  if not model or model == "" then
    return nil
  end

  local ok, config = pcall(require, "loopbiotic.config")
  local overrides = ok and config.values and config.values.pricing or nil
  if overrides and overrides[model] then
    return overrides[model]
  end
  if M.rates[model] then
    return M.rates[model]
  end
  if overrides then
    for id, rate in pairs(overrides) do
      if model:find(id, 1, true) then
        return rate
      end
    end
  end
  for id, rate in pairs(M.rates) do
    if model:find(id, 1, true) then
      return rate
    end
  end

  return nil
end

-- Estimated USD cost for one usage record. `input_tokens` is the full input
-- (cached + fresh); the cached slice bills at the cheaper cache-read rate.
function M.cost(usage, model)
  local rate = M.rate_for(model)
  if not rate then
    return nil
  end

  local input = tonumber(usage.input_tokens) or 0
  local cached = tonumber(usage.cached_input_tokens) or 0
  local output = tonumber(usage.output_tokens) or 0
  local fresh = math.max(input - cached, 0)

  return fresh * rate.input / 1e6 + cached * rate.cached_input / 1e6 + output * rate.output / 1e6
end

function M.format(cost)
  if not cost then
    return nil
  end
  if cost < 0.01 then
    return string.format("$%.4f", cost)
  end
  return string.format("$%.2f", cost)
end

return M
