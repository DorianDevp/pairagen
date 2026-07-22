use std::path::{Component, Path, PathBuf};

use loopbiotic_protocol::{ProjectArea, ProjectCommand, ProjectSignals, ProjectTechnology};

use super::{AdapterOutput, ProjectAdapter, RootFacts};

const ROOT_FILES: &[&str] = &["go.work", "go.mod"];
const MAX_AREAS: usize = 96;
const MAX_DEPENDENCIES: usize = 256;

pub(super) struct GoWorkspaceAdapter;

impl ProjectAdapter for GoWorkspaceAdapter {
    fn id(&self) -> &'static str {
        "go-workspace"
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        facts.has("go.work") || facts.has("go.mod")
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        let source = if facts.has("go.work") {
            PathBuf::from("go.work")
        } else {
            PathBuf::from("go.mod")
        };
        let areas = project_areas(facts);
        let (version, version_source) = go_version(facts, &areas)
            .map(|(version, source)| (Some(version), source))
            .unwrap_or_else(|| (None, source.clone()));
        let commands = go_test_command(facts, &areas)
            .map(|command| ProjectCommand {
                name: "go test".into(),
                command,
                source: source.clone(),
            })
            .into_iter()
            .collect();

        AdapterOutput {
            ecosystem: Some("go".into()),
            workspace_kind: Some((
                10,
                if facts.has("go.work") {
                    "go_workspace".into()
                } else {
                    "go_module".into()
                },
            )),
            technologies: vec![ProjectTechnology {
                name: "Go".into(),
                version,
                role: "language".into(),
                source: version_source,
            }],
            areas,
            commands,
            ..AdapterOutput::default()
        }
    }
}

fn go_test_command(facts: &RootFacts, areas: &[ProjectArea]) -> Option<String> {
    if !facts.has("go.work") {
        return Some("go test ./...".into());
    }
    let patterns = areas
        .iter()
        .map(|area| {
            if area.path == Path::new(".") {
                "./...".into()
            } else {
                format!("./{}/...", area.path.to_string_lossy().replace('\\', "/"))
            }
        })
        .collect::<Vec<String>>();
    (!patterns.is_empty()).then(|| format!("go test {}", patterns.join(" ")))
}

fn project_areas(facts: &RootFacts) -> Vec<ProjectArea> {
    let paths = if let Some(workspace) = facts.text("go.work") {
        directive_entries(workspace, "use", MAX_AREAS)
            .into_iter()
            .filter_map(|entry| first_field(&entry))
            .filter_map(|path| workspace_path(&path))
            .collect::<Vec<_>>()
    } else {
        vec![PathBuf::from(".")]
    };

    let mut areas = paths
        .into_iter()
        .filter_map(|path| module_area(facts, path))
        .collect::<Vec<_>>();
    areas.sort_by(|left, right| left.path.cmp(&right.path));
    areas.dedup_by(|left, right| left.path == right.path);
    areas
}

fn module_area(facts: &RootFacts, path: PathBuf) -> Option<ProjectArea> {
    let manifest_path = module_manifest_path(&path);
    let manifest = facts.read(&manifest_path)?;
    let name = directive_entries(&manifest, "module", 1)
        .into_iter()
        .next()
        .and_then(|entry| first_field(&entry))
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace")
                .to_string()
        });
    let mut dependencies = directive_entries(&manifest, "require", MAX_DEPENDENCIES)
        .into_iter()
        .filter_map(|entry| requirement(&entry))
        .collect::<Vec<_>>();
    dependencies.sort();
    dependencies.dedup();

    Some(ProjectArea {
        name,
        path,
        role: "go_module".into(),
        technologies: vec!["Go".into()],
        dependencies,
    })
}

fn go_version(facts: &RootFacts, areas: &[ProjectArea]) -> Option<(String, PathBuf)> {
    if let Some(manifest) = facts.text("go.work") {
        if let Some(version) = manifest_version(manifest) {
            return Some((version, PathBuf::from("go.work")));
        }
    } else if let Some(manifest) = facts.text("go.mod") {
        return manifest_version(manifest).map(|version| (version, PathBuf::from("go.mod")));
    }
    areas.iter().find_map(|area| {
        let source = module_manifest_path(&area.path);
        let manifest = facts.read(&source)?;
        manifest_version(&manifest).map(|version| (version, source))
    })
}

fn module_manifest_path(path: &Path) -> PathBuf {
    if path == Path::new(".") {
        PathBuf::from("go.mod")
    } else {
        path.join("go.mod")
    }
}

fn manifest_version(manifest: &str) -> Option<String> {
    let toolchain = directive_entries(manifest, "toolchain", 1)
        .into_iter()
        .next()
        .and_then(|entry| first_field(&entry))
        .filter(|toolchain| toolchain != "default");
    toolchain
        .map(|value| format!("toolchain {value}"))
        .or_else(|| {
            directive_entries(manifest, "go", 1)
                .into_iter()
                .next()
                .and_then(|entry| first_field(&entry))
                .map(|value| format!("go {value}"))
        })
}

fn requirement(entry: &str) -> Option<String> {
    let mut fields = entry.split_whitespace();
    let module = fields.next()?.trim_matches(['"', '`']);
    let version = fields.next()?.trim_matches(['"', '`']);
    if module.is_empty() || version.is_empty() {
        return None;
    }
    Some(format!("{module}@{version}"))
}

fn workspace_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value.trim_matches(['"', '`']));
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if let Component::Normal(component) = component {
            normalized.push(component);
        }
    }
    Some(if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    })
}

fn first_field(value: &str) -> Option<String> {
    let field = value.split_whitespace().next()?.trim_matches(['"', '`']);
    (!field.is_empty()).then(|| field.to_string())
}

fn directive_entries(manifest: &str, directive: &str, limit: usize) -> Vec<String> {
    let mut entries = Vec::new();
    let mut in_block = false;
    for raw_line in manifest.lines() {
        if entries.len() >= limit {
            break;
        }
        let line = raw_line.split_once("//").map_or(raw_line, |(line, _)| line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if in_block {
            if line == ")" {
                in_block = false;
            } else {
                entries.push(line.to_string());
            }
            continue;
        }
        let Some(rest) = line.strip_prefix(directive) else {
            continue;
        };
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let rest = rest.trim();
        if rest == "(" {
            in_block = true;
        } else if !rest.is_empty() {
            entries.push(rest.to_string());
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_block_and_single_line_directives() {
        let manifest = r#"
module example.com/service
go 1.24.0
require example.com/direct v1.2.3
require (
    example.com/first v2.0.0 // indirect
    example.com/second v3.1.0
)
"#;

        assert_eq!(
            directive_entries(manifest, "require", MAX_DEPENDENCIES),
            [
                "example.com/direct v1.2.3",
                "example.com/first v2.0.0",
                "example.com/second v3.1.0"
            ]
        );
        assert_eq!(manifest_version(manifest).as_deref(), Some("go 1.24.0"));
    }

    #[test]
    fn rejects_workspace_paths_that_escape_the_root() {
        assert_eq!(
            workspace_path("./services/api"),
            Some("services/api".into())
        );
        assert_eq!(workspace_path("."), Some(".".into()));
        assert_eq!(workspace_path("../outside"), None);
        assert_eq!(workspace_path("/outside"), None);
    }
}
