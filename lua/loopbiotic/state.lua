---@class LoopbioticGoal
---@field statement string
---@field completed_steps string[]
---@field known_observations table[]
---@field status string "idle" | "active" | "paused" | "complete"
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
---@field call_hierarchy table|nil session-pinned locally resolved Flow graph
---@field card_flow_active boolean Flow navigation owns the card keymaps
---@field diff_buf integer|nil draft buffer of the inline patch preview
---@field diff_win integer|nil
---@field diff_source_buf integer|nil
---@field diff_source_tick integer|nil changedtick guard for the draft
---@field diff_first_row integer|nil
---@field diff_cursor integer[]|nil
---@field thinking_timer userdata|nil
---@field thinking_frame integer|nil
---@field thinking_request_id string|nil
---@field thinking_session_id string|nil
---@field thinking_started_at integer|nil
---@field thinking_label string|nil
---@field thinking_steps table[]|nil
---@field thinking_preview table|nil non-actionable streamed { title, body? }
---@field token_usage LoopbioticTokenUsage|nil
---@field turn_token_usage LoopbioticTokenUsage|nil
---@field backend_model string|nil model the backend reported using
---@field backend_models table|nil last backend-reported model per phase: { patch?, discovery? }
---@field agent_identity table|nil backend/warmup identity: { backend, model, models }
---@field backend_preflight_error string|nil last backend/warmup error; nil once a warmup or turn succeeds
---@field prompt_stash string|nil composed prompt text preserved across a failed session start
---@field prompt_stash_mode string|nil mode preserved with a failed submitted prompt
---@field session_mode string|nil mode used by the active session's latest submitted prompt
---@field last_backend_error string|nil message of the last backend error card, for repeat escalation
---@field context_report table|nil
---@field workspace_hints table[]|nil
---@field details_card LoopbioticCard|nil
---@field details_expanded boolean
---@field cancelled_turn_id string|nil
---@field turn_barrier boolean true while an interrupted backend turn is settling
---@field pending_widget_context table<string, table> visible context selected in AgentWindow Widgets
---@field creation table|nil validated pending new-file plan
---@field file_ops table|nil validated pending file-operation plan (moves)
---@field skills_root string|nil workspace root for the session instruction catalog
---@field instruction_skill_catalog table[] safe Markdown candidates
---@field selected_instruction_skills table<string, boolean> session-scoped selection
---@field surfaces table authoritative PromptWindow and AgentWindow singleton state
---@field reset fun()

-- Every mutable field with its initial value. Fields that start as nil list
-- vim.NIL here so reset() can restore them without losing the key.
local defaults = {
  session_id = vim.NIL,
  source_buf = vim.NIL,
  source_cursor = vim.NIL,
  card = vim.NIL,
  goal = vim.NIL,
  call_hierarchy = vim.NIL,
  card_flow_active = false,
  diff_buf = vim.NIL,
  diff_win = vim.NIL,
  diff_source_buf = vim.NIL,
  diff_source_tick = vim.NIL,
  diff_first_row = vim.NIL,
  diff_cursor = vim.NIL,
  thinking_timer = vim.NIL,
  thinking_frame = vim.NIL,
  thinking_request_id = vim.NIL,
  thinking_session_id = vim.NIL,
  thinking_started_at = vim.NIL,
  thinking_label = vim.NIL,
  thinking_steps = vim.NIL,
  thinking_preview = vim.NIL,
  token_usage = vim.NIL,
  turn_token_usage = vim.NIL,
  backend_model = vim.NIL,
  backend_models = vim.NIL,
  agent_identity = vim.NIL,
  backend_preflight_error = vim.NIL,
  prompt_stash = vim.NIL,
  prompt_stash_mode = vim.NIL,
  session_mode = vim.NIL,
  last_backend_error = vim.NIL,
  context_report = vim.NIL,
  workspace_hints = vim.NIL,
  details_card = vim.NIL,
  details_expanded = false,
  cancelled_turn_id = vim.NIL,
  turn_barrier = false,
  pending_widget_context = {},
  creation = vim.NIL,
  creation_context_win = vim.NIL,
  file_ops = vim.NIL,
  skills_root = vim.NIL,
  instruction_skill_catalog = {},
  selected_instruction_skills = {},
  surfaces = {
    prompt = {
      mode = "closed",
    },
    agent = {
      mode = "closed",
      working = false,
      cursorline = false,
    },
  },
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
