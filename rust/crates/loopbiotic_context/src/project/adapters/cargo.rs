use std::path::PathBuf;

use loopbiotic_protocol::{ProjectArea, ProjectCommand, ProjectSignals, ProjectTechnology};

use super::{AdapterOutput, ProjectAdapter, RootFacts};

const ROOT_FILES: &[&str] = &["Cargo.toml", "Cargo.lock"];

pub(super) struct CargoWorkspaceAdapter;

impl ProjectAdapter for CargoWorkspaceAdapter {
    fn id(&self) -> &'static str {
        "cargo-workspace"
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        facts.has("Cargo.toml")
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        let cargo = facts.text("Cargo.toml").unwrap_or_default();
        let edition = quoted_assignment(cargo, "edition").map(|value| format!("edition {value}"));
        AdapterOutput {
            ecosystem: Some("rust".into()),
            workspace_kind: Some((10, "rust_workspace".into())),
            technologies: vec![ProjectTechnology {
                name: "Rust".into(),
                version: edition,
                role: "language".into(),
                source: PathBuf::from("Cargo.toml"),
            }],
            areas: workspace_members(cargo),
            commands: vec![ProjectCommand {
                name: "cargo test".into(),
                command: "cargo test --workspace".into(),
                source: PathBuf::from("Cargo.toml"),
            }],
            ..AdapterOutput::default()
        }
    }
}

pub(super) struct CargoTechnologyAdapter {
    id: &'static str,
    dependency: &'static str,
    name: &'static str,
    role: &'static str,
}

impl CargoTechnologyAdapter {
    pub const fn new(
        id: &'static str,
        dependency: &'static str,
        name: &'static str,
        role: &'static str,
    ) -> Self {
        Self {
            id,
            dependency,
            name,
            role,
        }
    }
}

impl ProjectAdapter for CargoTechnologyAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        facts
            .text("Cargo.toml")
            .and_then(|cargo| dependency_version(cargo, self.dependency))
            .is_some()
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        AdapterOutput {
            technologies: vec![ProjectTechnology {
                name: self.name.into(),
                version: facts
                    .text("Cargo.toml")
                    .and_then(|cargo| dependency_version(cargo, self.dependency)),
                role: self.role.into(),
                source: PathBuf::from("Cargo.toml"),
            }],
            ..AdapterOutput::default()
        }
    }
}

fn quoted_assignment(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix(key)?.trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        Some(rest.strip_prefix('"')?.split('"').next()?.to_string())
    })
}

fn dependency_version(text: &str, dependency: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix(dependency)?.trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        if let Some(version) = rest.strip_prefix('"') {
            return version.split('"').next().map(str::to_string);
        }
        let version = rest.split("version").nth(1)?.trim_start();
        let version = version.strip_prefix('=')?.trim_start().strip_prefix('"')?;
        version.split('"').next().map(str::to_string)
    })
}

fn workspace_members(text: &str) -> Vec<ProjectArea> {
    let Some(start) = text.find("members") else {
        return Vec::new();
    };
    let Some(open) = text[start..].find('[').map(|index| start + index) else {
        return Vec::new();
    };
    let Some(close) = text[open..].find(']').map(|index| open + index) else {
        return Vec::new();
    };
    text[open + 1..close]
        .split(',')
        .take(96)
        .filter_map(|value| {
            let path = value.trim().trim_matches('"');
            if path.is_empty() || path.contains('*') {
                return None;
            }
            let path = PathBuf::from(path);
            Some(ProjectArea {
                name: path.file_name()?.to_string_lossy().into_owned(),
                role: if path.starts_with("apps") {
                    "application".into()
                } else {
                    "rust_crate".into()
                },
                path,
                technologies: vec!["Rust".into()],
                dependencies: vec![],
            })
        })
        .collect()
}
