---@class LoopbioticGoal
---@field statement string
---@field completed_steps string[]
---@field known_observations table[]
---@field status string "active" | "needs_review" | "complete"
---@field next_step? string

---@class LoopbioticTokenUsage
---@field input_tokens? integer
---@field cached_input_tokens? integer
---@field output_tokens? integer
---@field total_tokens? integer
---@field estimated? boolean

---@class LoopbioticState
---@field session_id string|nil active backend session
---@field source_buf integer|nil buffer the session was started from
---@field source_cursor integer[]|nil { line, column } (0-based column)
---@field card LoopbioticCard|nil card currently shown
---@field goal LoopbioticGoal|nil
---@field prompt_win integer|nil
---@field prompt_buf integer|nil
---@field prompt_frame_win integer|nil
---@field prompt_frame_buf integer|nil
---@field card_win integer|nil
---@field card_buf integer|nil
---@field status_win integer|nil
---@field status_buf integer|nil
---@field diff_tab integer|nil
---@field diff_buf integer|nil draft buffer of the inline patch preview
---@field diff_win integer|nil
---@field diff_source_buf integer|nil
---@field diff_source_tick integer|nil changedtick guard for the draft
---@field diff_first_row integer|nil
---@field diff_cursor integer[]|nil
---@field thinking_win integer|nil
---@field thinking_buf integer|nil
---@field thinking_timer userdata|nil
---@field thinking_frame integer|nil
---@field thinking_request_id string|nil
---@field thinking_session_id string|nil
---@field thinking_started_at integer|nil
---@field thinking_label string|nil
---@field thinking_steps table[]|nil
---@field last_card LoopbioticCard|nil
---@field token_usage LoopbioticTokenUsage|nil
---@field turn_token_usage LoopbioticTokenUsage|nil
---@field backend_model string|nil model the backend reported using
---@field context_report table|nil
---@field workspace_hints table[]|nil
---@field completion_notified_card string|nil
---@field completion_checked_card string|nil
---@field details_card LoopbioticCard|nil
---@field details_expanded boolean
---@field navigated_card LoopbioticCard|nil
---@field reset fun()

-- Every mutable field with its initial value. Fields that start as nil list
-- vim.NIL here so reset() can restore them without losing the key.
local defaults = {
  session_id = vim.NIL,
  source_buf = vim.NIL,
  source_cursor = vim.NIL,
  card = vim.NIL,
  goal = vim.NIL,
  prompt_win = vim.NIL,
  prompt_buf = vim.NIL,
  prompt_frame_win = vim.NIL,
  prompt_frame_buf = vim.NIL,
  card_win = vim.NIL,
  card_buf = vim.NIL,
  status_win = vim.NIL,
  status_buf = vim.NIL,
  diff_tab = vim.NIL,
  diff_buf = vim.NIL,
  diff_win = vim.NIL,
  diff_source_buf = vim.NIL,
  diff_source_tick = vim.NIL,
  diff_first_row = vim.NIL,
  diff_cursor = vim.NIL,
  thinking_win = vim.NIL,
  thinking_buf = vim.NIL,
  thinking_timer = vim.NIL,
  thinking_frame = vim.NIL,
  thinking_request_id = vim.NIL,
  thinking_session_id = vim.NIL,
  thinking_started_at = vim.NIL,
  thinking_label = vim.NIL,
  thinking_steps = vim.NIL,
  last_card = vim.NIL,
  token_usage = vim.NIL,
  turn_token_usage = vim.NIL,
  backend_model = vim.NIL,
  context_report = vim.NIL,
  workspace_hints = vim.NIL,
  completion_notified_card = vim.NIL,
  completion_checked_card = vim.NIL,
  details_card = vim.NIL,
  details_expanded = false,
  navigated_card = vim.NIL,
}

---@type LoopbioticState
local M = {}

-- Restore every field to its initial value. Non-state cleanup (timers,
-- windows, the RPC job) is the caller's job; see require("loopbiotic").reset().
function M.reset()
  for field, value in pairs(defaults) do
    if value == vim.NIL then
      M[field] = nil
    else
      M[field] = vim.deepcopy(value)
    end
  end
end

M.reset()

return M
