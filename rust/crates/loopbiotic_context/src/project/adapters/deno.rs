use std::fs;
use std::path::{Path, PathBuf};

use loopbiotic_protocol::{ProjectCommand, ProjectSignals, ProjectTechnology};
use serde_json::Value;

use super::{AdapterOutput, ProjectAdapter, RootFacts};

const ROOT_FILES: &[&str] = &["deno.json", "deno.jsonc", "deno.lock"];

pub(super) struct DenoAdapter;

impl ProjectAdapter for DenoAdapter {
    fn id(&self) -> &'static str {
        "deno"
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        facts.has("deno.json") || facts.has("deno.jsonc") || facts.has("deno.lock")
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        let manifest = facts.json("deno.json");
        let commands = manifest
            .as_ref()
            .and_then(|value| value.get("tasks"))
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .take(64)
            .map(|(name, _)| ProjectCommand {
                name: format!("deno {name}"),
                command: format!("deno task {name}"),
                source: PathBuf::from("deno.json"),
            })
            .collect();
        let (version, source) = find_deno_image(&facts.root)
            .map(|(version, path)| (Some(version), path))
            .unwrap_or((None, PathBuf::from("deno.json")));
        AdapterOutput {
            ecosystem: Some("javascript".into()),
            technologies: vec![ProjectTechnology {
                name: "Deno".into(),
                version,
                role: "runtime_and_package_manager".into(),
                source,
            }],
            commands,
            ..AdapterOutput::default()
        }
    }
}

fn find_deno_image(root: &Path) -> Option<(String, PathBuf)> {
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0;
    while let Some(directory) = stack.pop() {
        if visited >= 2_000 {
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
            } else if kind.is_file() && name.ends_with("Dockerfile") {
                let Ok(relative) = entry.path().strip_prefix(root).map(Path::to_path_buf) else {
                    continue;
                };
                let Ok(content) = fs::read_to_string(entry.path()) else {
                    continue;
                };
                if let Some(start) = content.find("denoland/deno:") {
                    let version = content[start + "denoland/deno:".len()..]
                        .split(|character: char| character.is_whitespace() || character == '@')
                        .next()?;
                    return Some((version.to_string(), relative));
                }
            }
        }
    }
    None
}
