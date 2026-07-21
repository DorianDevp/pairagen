use loopbiotic_protocol::{ProjectSignals, ProjectTool};

use super::{AdapterOutput, ProjectAdapter, RootFacts};

pub(super) struct NeovimLspAdapter;

impl ProjectAdapter for NeovimLspAdapter {
    fn id(&self) -> &'static str {
        "neovim-lsp"
    }

    fn root_files(&self) -> &'static [&'static str] {
        &[]
    }

    fn matches(&self, _facts: &RootFacts, signals: &ProjectSignals) -> bool {
        !signals.lsp_clients.is_empty()
    }

    fn inspect(&self, _facts: &RootFacts, signals: &ProjectSignals) -> AdapterOutput {
        AdapterOutput {
            tools: signals
                .lsp_clients
                .iter()
                .map(|client| ProjectTool {
                    name: client.name.clone(),
                    role: "language_server".into(),
                    source: "neovim".into(),
                    version: client.version.clone(),
                    root: client.root.clone(),
                    capabilities: client.capabilities.clone(),
                })
                .collect(),
            ..AdapterOutput::default()
        }
    }
}
