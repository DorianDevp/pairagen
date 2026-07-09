pub mod generic;
pub mod mock;
pub mod stdio_agent;

use anyhow::Result;
use async_trait::async_trait;
use pair_protocol::{Action, BackendInfo, Card, ContextBundle};
use serde::Serialize;

pub use generic::*;
pub use mock::*;
pub use stdio_agent::*;

#[async_trait]
pub trait BackendAdapter: Send + Sync {
    async fn next_card(&self, req: BackendRequest) -> Result<BackendResponse>;

    fn capabilities(&self) -> BackendInfo;
}

#[derive(Clone, Debug)]
pub struct BackendRequest {
    pub session: SessionSnapshot,
    pub action: BackendAction,
    pub context: ContextBundle,
    pub card_contract: CardContract,
}

#[derive(Clone, Debug)]
pub enum BackendAction {
    Start,
    User(Action),
}

#[derive(Clone, Debug, Serialize)]
pub struct CardContract {
    pub one_card_only: bool,
    pub patch_only_on_fix: bool,
    pub max_body_chars: usize,
}

impl Default for CardContract {
    fn default() -> Self {
        Self {
            one_card_only: true,
            patch_only_on_fix: true,
            max_body_chars: 1_200,
        }
    }
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
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub prompt: String,
    pub card_count: usize,
    pub last_card: Option<Card>,
}
