use std::fs;
use std::path::{Path, PathBuf};

use loopbiotic_protocol::{ProjectArea, ProjectSignals, ProjectTechnology};
use serde_json::Value;

use super::{AdapterOutput, ProjectAdapter, RootFacts};

const ROOT_FILES: &[&str] = &["nx.json", "package.json", "deno.lock"];

pub(super) struct NxAdapter;

impl ProjectAdapter for NxAdapter {
    fn id(&self) -> &'static str {
        "nx"
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        facts.has("nx.json")
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        AdapterOutput {
            ecosystem: Some("javascript".into()),
            workspace_kind: Some((20, "nx_workspace".into())),
            technologies: vec![ProjectTechnology {
                name: "Nx".into(),
                version: nx_version(facts),
                role: "workspace_orchestrator".into(),
                source: PathBuf::from(if facts.has("deno.lock") {
                    "deno.lock"
                } else {
                    "package.json"
                }),
            }],
            areas: project_areas(facts),
            ..AdapterOutput::default()
        }
    }
}

fn nx_version(facts: &RootFacts) -> Option<String> {
    facts
        .locked_npm_version("nx")
        .or_else(|| facts.package_dependency("nx"))
        .map(str::to_string)
}

fn project_areas(facts: &RootFacts) -> Vec<ProjectArea> {
    let mut stack = vec![facts.root.clone()];
    let mut files = Vec::new();
    let mut visited = 0;
    while let Some(directory) = stack.pop() {
        if visited >= 2_000 || files.len() >= 96 {
            break;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir()
                && !matches!(name.as_ref(), ".git" | "node_modules" | "target" | "dist")
            {
                stack.push(entry.path());
            } else if kind.is_file() && name == "project.json" {
                if let Ok(relative) = entry.path().strip_prefix(&facts.root) {
                    files.push(relative.to_path_buf());
                }
            }
            if visited >= 2_000 || files.len() >= 96 {
                break;
            }
        }
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|relative| {
            let project: Value = serde_json::from_str(&facts.read(&relative)?).ok()?;
            let name = project.get("name")?.as_str()?.to_string();
            let path = project
                .get("sourceRoot")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| relative.parent().unwrap_or(Path::new(".")).to_path_buf());
            let serialized = project.to_string();
            let mut technologies = vec!["TypeScript".into(), "Nx".into()];
            if path.to_string_lossy().contains("angular") || serialized.contains("@angular/") {
                technologies.push("Angular".into());
            }
            if path.to_string_lossy().contains("react") || serialized.contains("react") {
                technologies.push("React".into());
            }
            let dependencies = project
                .get("implicitDependencies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            Some(ProjectArea {
                name,
                path,
                role: project
                    .get("projectType")
                    .and_then(Value::as_str)
                    .unwrap_or("workspace_project")
                    .into(),
                technologies,
                dependencies,
            })
        })
        .collect()
}
