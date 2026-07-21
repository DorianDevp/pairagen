pub mod claude_app;
pub mod codex_app;
pub mod generic;
pub mod mock;
pub mod ollama;
pub mod openai_compatible;
pub mod stdio_agent;
pub mod stream;
pub(crate) mod support;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use loopbiotic_protocol::{
    Action, BackendInfo, Card, CardKind, ContextBundle, ErrorCard, InstructionSkill,
    MAX_CHANGED_LINES, MAX_HUNKS_PER_PATCH, MAX_PATCH_FILES, Mode, ProjectProfile, TokenUsage,
};
use serde::Serialize;
use serde_json::json;

pub use claude_app::*;
pub use codex_app::*;
pub use generic::*;
pub use mock::*;
pub use ollama::*;
pub use openai_compatible::*;
pub use stdio_agent::*;
pub use stream::*;

/// Shared implementation policy injected into every patch-capable backend.
/// Prompt guidance is backed by the patch validator's one-change-run gate;
/// the dependency-order clauses cover compiler errors that cannot be inferred
/// reliably from language-agnostic diff syntax alone.
pub const IMPLEMENTATION_GUIDELINES: &str = "Compiler acceptance is a hard invariant for every patch. Applying this one proposed patch by itself to the currently accepted source must leave the project compiling and type-checking; never rely on a later patch to repair undefined symbols, missing declarations or imports, incompatible signatures, producer/consumer mismatches, schema drift, or an incomplete refactor. Order work by dependencies: introduce each declaration, type, interface, function, import, field, and compatibility shim in an independently compiler-valid patch before any later patch first references, implements, or depends on it. For interface or named-type extraction, the first patch contains ONLY the independently valid declaration and leaves every existing use byte-for-byte unchanged; only after that declaration patch is accepted may a later patch replace the inline type or implement the interface. A declaration-only preparation must also satisfy the project's unused-declaration compiler and lint rules; use the project's appropriate exported/public/annotation mechanism, or return choice/deny when no clean standalone declaration is valid. Never combine a declaration and its separated consumer change on one card, even when both fit inside one @@ header. Emit exactly one uninterrupted change block. In structured hunk lines, after the first add/remove record, a context record ends the change block and no later add/remove record is allowed. Prefer backward-compatible preparation such as declarations, adapters, overloads, defaults, or optional fields before changing consumers. If no compiler-valid intermediate state fits one uninterrupted block, return a blocking choice or deny instead of broken code or a batch.";

/// Contract shared by every backend for the editor-resolved static Flow
/// graph. Keeping this text identical prevents adapters from silently
/// spending discovery tools on information the LSP already supplied.
pub const FLOW_GUIDELINES: &str = "When ctx.call_hierarchy is present, treat it as the complete locally resolved static Flow graph within its explicit limits. Use its caller-to-callee edges, exact call-site ranges, reference counts, partial/truncated flags, and snippets to explain code flow, assess impact, choose the change location, plan tests, refactor, and navigate call-sites. Do not use tools or searches to re-enumerate the same callers or callees and do not reconstruct a competing callstack. When the user asks to show or explain a callstack, call path, or code flow, return a hypothesis or finding with flow_path containing every available node id on the requested path in caller-to-callee order; never invent an id. Return an empty flow_path for other answers. A partial, truncated, or unavailable graph is an explicit boundary, not permission for agent-side call-hierarchy discovery; other supplied context and references remain usable.";

#[async_trait]
pub trait BackendAdapter: Send + Sync {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse>;

    async fn next_card_with_progress(
        &self,
        req: BackendRequest,
        _progress: Option<ProgressReporter>,
    ) -> Result<BackendResponse> {
        self.next_card(req).await
    }

    /// Called when the editor opens the prompt window, before any session
    /// exists, so backends can pay their startup cost while the user types.
    async fn warmup(&self) -> Result<()> {
        Ok(())
    }

    /// Interrupts in-flight work for one Loopbiotic session. Persistent
    /// backends override this so cancelling a Working card stops the actual
    /// agent, not only the daemon future waiting for it.
    async fn cancel_turn(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }

    /// Reports who will answer the next turn: the adapter name, the concrete
    /// model it resolves to (configured, else discovered, else unknown), and
    /// any models the backend can enumerate for switching.
    async fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            backend: self.capabilities().name,
            model: None,
            models: vec![],
            phases: None,
        }
    }

    fn capabilities(&self) -> BackendInfo;
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendIdentity {
    pub backend: String,
    pub model: Option<String>,
    pub models: Vec<String>,
    /// Set when the backend runs different models per turn phase; `model`
    /// then names the patch-phase model (the one that writes code).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phases: Option<BackendPhaseModels>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendPhaseModels {
    pub discovery: Option<String>,
    pub patch: Option<String>,
}

pub type ProgressReporter = Arc<dyn Fn(BackendProgress) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackendPreview {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendProgress {
    pub session_id: String,
    pub phase: String,
    pub message: String,
    /// Non-actionable content extracted from an incomplete structured
    /// response. The editor may display it while the full card is still being
    /// parsed and validated, but must not expose final-card actions yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<BackendPreview>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendRequest {
    pub session: SessionSnapshot,
    pub action: BackendAction,
    pub context: ContextBundle,
    pub card_contract: CardContract,
}

pub fn backend_context(context: &ContextBundle) -> serde_json::Value {
    let artifacts = context
        .artifacts
        .iter()
        .map(|artifact| {
            json!({
                "file": artifact.file,
                "start_line": artifact.start_line,
                "end_line": artifact.end_line,
                "kind": artifact.kind,
                "reason": artifact.reason,
                "text": artifact.text,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "cwd": context.cwd,
        "file": context.file,
        "cursor": context.cursor,
        "selection": context.selection,
        "buffer_text": context.buffer_text,
        "buffer_start_line": context.buffer_start_line,
        "diagnostics": context.diagnostics,
        "artifacts": artifacts,
        "call_hierarchy": context.call_hierarchy,
    })
}

#[derive(Clone, Debug, Serialize)]
pub enum BackendAction {
    Start,
    User(Action),
    Reply(String),
    ContractRetry(String),
    // The editor granted an open_location request mid-turn; the request's
    // context carries the freshly opened buffer.
    LocationGranted,
}

#[derive(Clone, Debug, Serialize)]
pub struct CardContract {
    pub one_card_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_kind: Option<CardKind>,
    pub allow_goal_completion: bool,
    /// Conversational turns may answer, point to evidence, or ask a question,
    /// but must never return a patch or completion summary.
    pub conversation_only: bool,
    pub max_body_chars: usize,
    pub max_patch_files: usize,
    pub max_hunks_per_patch: usize,
    pub max_changed_lines: usize,
}

impl Default for CardContract {
    fn default() -> Self {
        Self {
            one_card_only: true,
            expected_kind: None,
            allow_goal_completion: false,
            conversation_only: false,
            max_body_chars: 1_200,
            max_patch_files: MAX_PATCH_FILES,
            max_hunks_per_patch: MAX_HUNKS_PER_PATCH,
            max_changed_lines: MAX_CHANGED_LINES,
        }
    }
}

/// Card id marking an error card that stands in for backend output which could
/// not be parsed as a Loopbiotic op. The engine treats these as repairable
/// contract violations (retry with a strict-JSON instruction) rather than
/// terminal backend errors.
pub const UNPARSED_OUTPUT_CARD_ID: &str = "c_backend_unparsed_output";

pub fn enforce_card_contract(
    card: Card,
    contract: &CardContract,
    backend: &str,
    raw_output: &str,
) -> Card {
    let Some(expected_kind) = contract.expected_kind else {
        if contract.conversation_only
            && matches!(card, Card::Patch(_) | Card::Summary(_) | Card::Working(_))
        {
            return Card::Error(ErrorCard {
                id: "c_backend_contract_error".into(),
                title: "Backend returned work during conversation".into(),
                message: format!(
                    "{backend} returned a {:?} card, but this turn requires a conversational response.",
                    card.kind()
                ),
                actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
            });
        }
        return card;
    };

    // A clarifying question is a legitimate alternative to guessing wherever a
    // discovery card is expected; patch and summary requests stay strict.
    let discovery_expected = matches!(expected_kind, CardKind::Hypothesis | CardKind::Finding);

    if matches!(card, Card::Error(_) | Card::Deny(_) | Card::OpenLocation(_))
        || card.kind() == expected_kind
        || (discovery_expected && matches!(card, Card::Choice(_)))
        || (contract.allow_goal_completion && matches!(card, Card::Summary(_)))
    {
        return card;
    }

    let received_kind = card.kind();
    let raw_output = excerpt(raw_output, contract.max_body_chars);
    let mut message = format!(
        "{backend} returned a {received_kind:?} card, but this request requires a {expected_kind:?} card."
    );

    if !raw_output.is_empty() {
        message.push_str("\n\nRaw backend response:\n");
        message.push_str(&raw_output);
    }

    Card::Error(ErrorCard {
        id: "c_backend_contract_error".into(),
        title: "Backend returned the wrong card type".into(),
        message,
        actions: vec![Action::Retry, Action::EditPrompt, Action::Stop],
    })
}

pub fn excerpt(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    let mut result = text.chars().take(max_chars).collect::<String>();

    if text.chars().count() > max_chars {
        result.push_str("\n...");
    }

    result
}

#[derive(Clone, Debug)]
pub struct BackendResponse {
    pub card: Card,
    pub raw_output: Option<String>,
    pub metadata: BackendMetadata,
}

#[derive(Clone, Debug)]
pub struct BackendMetadata {
    pub backend: String,
    pub model: Option<String>,
    pub token_usage: Option<TokenUsage>,
    pub activities: Vec<String>,
    pub attempts: Vec<loopbiotic_protocol::AgentAttempt>,
}

pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    let estimate = (chars / 4).max(words);

    estimate.max(1)
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interaction_feedback: Vec<String>,
    pub completed_steps: Vec<String>,
    pub known_observations: Vec<String>,
    pub mode: Mode,
    pub card_count: usize,
    pub last_card: Option<Card>,
    pub last_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<InstructionSkill>,
}

/// Length of the shared byte prefix of two prompts — the part a provider
/// prompt cache can reuse across the two requests.
#[cfg(test)]
pub(crate) fn common_prefix_len(a: &str, b: &str) -> usize {
    a.bytes()
        .zip(b.bytes())
        .take_while(|(left, right)| left == right)
        .count()
}

#[cfg(test)]
pub(crate) fn test_request() -> BackendRequest {
    BackendRequest {
        session: SessionSnapshot {
            id: "s_1".into(),
            prompt: "payload is empty".into(),
            interaction_feedback: vec![],
            completed_steps: vec![],
            known_observations: vec![],
            mode: Mode::Investigate,
            card_count: 0,
            last_card: None,
            last_summary: None,
            project: None,
            skills: vec![],
        },
        action: BackendAction::Start,
        context: ContextBundle {
            cwd: "/tmp/project".into(),
            file: "src/main.rs".into(),
            cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "fn main() {}".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
            call_hierarchy: None,
        },
        card_contract: CardContract::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopbiotic_protocol::{HypothesisCard, SummaryCard};

    struct BareAdapter;

    #[async_trait]
    impl BackendAdapter for BareAdapter {
        async fn next_card(&self, _req: BackendRequest) -> Result<BackendResponse> {
            unreachable!("identity tests never run a turn")
        }

        fn capabilities(&self) -> BackendInfo {
            BackendInfo {
                name: "bare".into(),
                streaming: false,
                patches: false,
                reasoning: false,
                can_read_project: false,
                can_use_tools: false,
            }
        }
    }

    #[tokio::test]
    async fn default_identity_reports_the_adapter_name_without_a_model() {
        let identity = BareAdapter.identity().await;

        assert_eq!(identity.backend, "bare");
        assert_eq!(identity.model, None);
        assert!(identity.models.is_empty());
    }

    fn hypothesis() -> Card {
        Card::Hypothesis(HypothesisCard {
            id: "c_hypothesis".into(),
            title: "Hypothesis".into(),
            claim: "The response has the wrong type.".into(),
            evidence: None,
            next_move: None,
            flow_path: vec![],
            actions: vec![Action::Fix],
        })
    }

    #[test]
    fn rejects_non_patch_when_patch_is_required() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Patch),
            ..CardContract::default()
        };
        let card =
            enforce_card_contract(hypothesis(), &contract, "Codex", "{\"op\":\"hypothesis\"}");

        let Card::Error(error) = card else {
            panic!("expected contract error card");
        };

        assert!(error.message.contains("Hypothesis card"));
        assert!(error.message.contains("Patch card"));
        assert!(error.message.contains("Raw backend response"));
    }

    #[test]
    fn allows_the_required_card_type() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Hypothesis),
            ..CardContract::default()
        };

        assert!(matches!(
            enforce_card_contract(hypothesis(), &contract, "Codex", "{}"),
            Card::Hypothesis(_)
        ));
    }

    #[test]
    fn allows_choice_when_discovery_kind_is_expected() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Hypothesis),
            ..CardContract::default()
        };
        let choice = Card::Choice(loopbiotic_protocol::ChoiceCard {
            id: "c_choice".into(),
            title: "Clarify".into(),
            question: "Which behavior do you want?".into(),
            options: vec![],
        });

        assert!(matches!(
            enforce_card_contract(choice, &contract, "Claude", "{}"),
            Card::Choice(_)
        ));
    }

    #[test]
    fn rejects_choice_when_patch_is_required() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Patch),
            ..CardContract::default()
        };
        let choice = Card::Choice(loopbiotic_protocol::ChoiceCard {
            id: "c_choice".into(),
            title: "Clarify".into(),
            question: "Which behavior do you want?".into(),
            options: vec![],
        });

        assert!(matches!(
            enforce_card_contract(choice, &contract, "Claude", "{}"),
            Card::Error(_)
        ));
    }

    #[test]
    fn allows_summary_for_goal_completion_contract() {
        let contract = CardContract {
            expected_kind: Some(CardKind::Patch),
            allow_goal_completion: true,
            ..CardContract::default()
        };
        let summary = Card::Summary(SummaryCard {
            id: "c_done".into(),
            title: "Goal complete".into(),
            summary: "The goal is resolved.".into(),
            changed_files: vec![],
            next_actions: vec![Action::Stop],
        });

        assert!(matches!(
            enforce_card_contract(summary, &contract, "test", "{}"),
            Card::Summary(_)
        ));
    }

    #[test]
    fn backend_context_excludes_optimizer_telemetry() {
        let context = ContextBundle {
            cwd: "/tmp/project".into(),
            file: "src/main.rs".into(),
            cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "fn main() {}".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![loopbiotic_protocol::ContextArtifact {
                file: "src/user.rs".into(),
                start_line: 3,
                end_line: 3,
                kind: loopbiotic_protocol::ContextArtifactKind::Definition,
                reason: "definition".into(),
                text: "struct User;".into(),
                estimated_tokens: 9,
                score: 240,
            }],
            report: Some(loopbiotic_protocol::ContextReport {
                enabled: true,
                candidate_count: 99,
                ..Default::default()
            }),
            call_hierarchy: Some(loopbiotic_protocol::CallHierarchy {
                root: None,
                nodes: vec![],
                edges: vec![],
                partial: false,
                truncated: false,
                unavailable: true,
            }),
        };

        let value = backend_context(&context);

        assert!(value.get("report").is_none());
        assert!(value.get("hints").is_none());
        assert_eq!(value["call_hierarchy"]["unavailable"], true);
        assert_eq!(value["artifacts"][0]["text"], "struct User;");
        assert!(value["artifacts"][0].get("score").is_none());
    }
}
