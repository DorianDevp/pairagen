use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

pub const MAX_INSTRUCTION_SKILLS: usize = 16;
pub const MAX_INSTRUCTION_SKILL_BYTES: usize = 65_536;
pub const MAX_INSTRUCTION_SKILLS_TOTAL_BYTES: usize = 262_144;

/// One user- or config-selected Markdown instruction attached to a session.
/// It is inert text: selecting it grants no tool or command capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstructionSkill {
    pub name: String,
    pub path: PathBuf,
    pub content: String,
    pub provenance: String,
    #[serde(default)]
    pub auto: bool,
    pub sha256: String,
}

/// A versioned technology fact derived from a project-owned manifest or lock.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectTechnology {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub role: String,
    pub source: PathBuf,
}

/// One project-local command discovered from a task or workspace manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectCommand {
    pub name: String,
    pub command: String,
    pub source: PathBuf,
}

/// A bounded area in a polyglot workspace. Technology names reference entries
/// in `ProjectProfile.technologies` and keep the backend packet compact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectArea {
    pub name: String,
    pub path: PathBuf,
    pub role: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub technologies: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

/// One editor-owned tool signal. Neovim reports attached clients and
/// capabilities; the Rust profiler decides how they contribute to the profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectLspClient {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Cheap editor facts supplied by the frontend. Filesystem inspection and
/// technology recognition remain backend-owned.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectSignals {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lsp_clients: Vec<ProjectLspClient>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectTool {
    pub name: String,
    pub role: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Deterministic facts produced by marker-activated backend adapters, including
/// bounded editor signals. This is evidence, not an execution grant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectProfile {
    pub schema_version: u32,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapters: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub technologies: Vec<ProjectTechnology>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<ProjectArea>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ProjectCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ProjectTool>,
}

pub fn validate_project_signals(signals: &ProjectSignals) -> Result<()> {
    if signals.lsp_clients.len() > 16 {
        return Err(anyhow!("project LSP client count exceeds 16"));
    }
    for client in &signals.lsp_clients {
        require_text("project LSP client name", &client.name)?;
        if let Some(root) = &client.root {
            require_relative_path("project LSP client root", root)?;
        }
        if client.capabilities.len() > 16 {
            return Err(anyhow!("project LSP capability count exceeds 16"));
        }
        for capability in &client.capabilities {
            require_text("project LSP capability", capability)?;
        }
    }
    Ok(())
}

pub fn validate_project_metadata(
    profile: Option<&ProjectProfile>,
    skills: &[InstructionSkill],
) -> Result<()> {
    if skills.len() > MAX_INSTRUCTION_SKILLS {
        return Err(anyhow!(
            "instruction skill count exceeds {MAX_INSTRUCTION_SKILLS}"
        ));
    }
    let mut total_skill_bytes = 0;
    for skill in skills {
        require_text("instruction skill name", &skill.name)?;
        require_relative_path("instruction skill path", &skill.path)?;
        require_text("instruction skill provenance", &skill.provenance)?;
        require_text("instruction skill sha256", &skill.sha256)?;
        if skill.content.len() > MAX_INSTRUCTION_SKILL_BYTES {
            return Err(anyhow!(
                "instruction skill {} exceeds {MAX_INSTRUCTION_SKILL_BYTES} bytes",
                skill.path.display()
            ));
        }
        total_skill_bytes += skill.content.len();
    }
    if total_skill_bytes > MAX_INSTRUCTION_SKILLS_TOTAL_BYTES {
        return Err(anyhow!(
            "instruction skills exceed {MAX_INSTRUCTION_SKILLS_TOTAL_BYTES} total bytes"
        ));
    }

    let Some(profile) = profile else {
        return Ok(());
    };
    if profile.schema_version != 1 {
        return Err(anyhow!(
            "unsupported project profile schema version {}",
            profile.schema_version
        ));
    }
    require_text("project kind", &profile.kind)?;
    for adapter in &profile.adapters {
        require_text("project adapter", adapter)?;
    }
    for technology in &profile.technologies {
        require_text("project technology name", &technology.name)?;
        require_text("project technology role", &technology.role)?;
        require_relative_path("project technology source", &technology.source)?;
    }
    for area in &profile.areas {
        require_text("project area name", &area.name)?;
        require_text("project area role", &area.role)?;
        require_relative_path("project area path", &area.path)?;
        for dependency in &area.dependencies {
            require_text("project area dependency", dependency)?;
        }
    }
    for command in &profile.commands {
        require_text("project command name", &command.name)?;
        require_text("project command", &command.command)?;
        require_relative_path("project command source", &command.source)?;
    }
    for tool in &profile.tools {
        require_text("project tool name", &tool.name)?;
        require_text("project tool role", &tool.role)?;
        require_text("project tool source", &tool.source)?;
        if let Some(root) = &tool.root {
            require_relative_path("project tool root", root)?;
        }
        for capability in &tool.capabilities {
            require_text("project tool capability", capability)?;
        }
    }

    Ok(())
}

fn require_text(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow!("{label} is empty"));
    }
    Ok(())
}

fn require_relative_path(label: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(anyhow!("{label} must remain inside the workspace"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_inert_workspace_relative_instruction_skills() {
        let skills = vec![InstructionSkill {
            name: "AGENTS.md".into(),
            path: "AGENTS.md".into(),
            content: "Keep changes local.".into(),
            provenance: "config".into(),
            auto: true,
            sha256: "abc".into(),
        }];

        validate_project_metadata(None, &skills).unwrap();
    }

    #[test]
    fn rejects_instruction_paths_outside_the_workspace() {
        let skills = vec![InstructionSkill {
            name: "outside".into(),
            path: "../outside.md".into(),
            content: "untrusted".into(),
            provenance: "workspace_root".into(),
            auto: false,
            sha256: "abc".into(),
        }];

        assert!(
            validate_project_metadata(None, &skills)
                .unwrap_err()
                .to_string()
                .contains("inside the workspace")
        );
    }

    #[test]
    fn rejects_an_oversized_instruction_set() {
        let skills = (0..5)
            .map(|index| InstructionSkill {
                name: format!("skill-{index}"),
                path: format!("skill-{index}.md").into(),
                content: "x".repeat(MAX_INSTRUCTION_SKILL_BYTES),
                provenance: "workspace_root".into(),
                auto: false,
                sha256: "abc".into(),
            })
            .collect::<Vec<_>>();

        assert!(
            validate_project_metadata(None, &skills)
                .unwrap_err()
                .to_string()
                .contains("total bytes")
        );
    }
}
