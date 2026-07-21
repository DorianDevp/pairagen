use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};

use super::SUBMIT_CARD_TOOL;
use super::responses::FunctionCall;

const MAX_READ_BYTES: usize = 32 * 1024;
const MAX_READ_FILE_BYTES: u64 = 512 * 1024;
const MAX_READ_LINES: usize = 400;
const MAX_SEARCH_FILES: usize = 4_096;
const MAX_SEARCH_FILE_BYTES: u64 = 512 * 1024;
const MAX_SEARCH_RESULTS: usize = 20;
const MAX_DIRECTORY_ENTRIES: usize = 200;

pub(super) struct ToolExecution {
    pub(super) output: String,
    pub(super) activity: String,
}

pub(super) fn definitions(include_reads: bool) -> Vec<Value> {
    let mut tools = Vec::new();
    if include_reads {
        tools.extend(read_definitions());
    }
    tools.push(json!({
        "type": "function",
        "name": SUBMIT_CARD_TOOL,
        "description": "Submit the one final Loopbiotic card described by the current input. Put the complete typed card object in card. Call this only after sufficient evidence has been collected.",
        "strict": false,
        "parameters": {
            "type": "object",
            "properties": {
                "card": {"type": "object", "additionalProperties": true}
            },
            "required": ["card"],
            "additionalProperties": false
        },
    }));
    tools
}

pub(super) fn execute(call: &FunctionCall, workspace: &Path) -> ToolExecution {
    match execute_inner(call, workspace) {
        Ok(execution) => execution,
        Err(error) => ToolExecution {
            output: json!({"ok": false, "error": error.to_string()}).to_string(),
            activity: "Rejected an unsafe local tool request".into(),
        },
    }
}

fn execute_inner(call: &FunctionCall, workspace: &Path) -> Result<ToolExecution> {
    match call.name.as_str() {
        "workspace_read_file" => read_file(&call.arguments, workspace),
        "workspace_search_text" => search_text(&call.arguments, workspace),
        "workspace_list_directory" => list_directory(&call.arguments, workspace),
        name => Err(anyhow!("unsupported local tool {name}")),
    }
}

fn read_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "name": "workspace_read_file",
            "description": "Read a bounded line range from one UTF-8 workspace file. Paths are workspace-relative.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": ["integer", "null"]},
                    "end_line": {"type": ["integer", "null"]}
                },
                "required": ["path", "start_line", "end_line"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "workspace_search_text",
            "description": "Search for a literal string in bounded UTF-8 workspace files. Optional path narrows the search to one directory or file.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "path": {"type": ["string", "null"]},
                    "max_results": {"type": ["integer", "null"]}
                },
                "required": ["query", "path", "max_results"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "workspace_list_directory",
            "description": "List one workspace directory without recursion. Paths are workspace-relative.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }
        }),
    ]
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

fn read_file(arguments: &str, workspace: &Path) -> Result<ToolExecution> {
    let args: ReadArgs = serde_json::from_str(arguments)?;
    let (root, file) = resolve_existing(workspace, &args.path)?;
    if !file.is_file() {
        return Err(anyhow!("path is not a regular file"));
    }
    let size = file
        .metadata()
        .map_err(|_| anyhow!("file metadata is unavailable"))?
        .len();
    if size > MAX_READ_FILE_BYTES {
        return Err(anyhow!(
            "file is {size} bytes, larger than the {MAX_READ_FILE_BYTES}-byte read limit"
        ));
    }
    let text = fs::read_to_string(&file).map_err(|_| anyhow!("file is not valid UTF-8"))?;
    let start = args.start_line.unwrap_or(1).max(1);
    let requested_end = args.end_line.unwrap_or(start + MAX_READ_LINES - 1);
    let end = requested_end.min(start + MAX_READ_LINES - 1).max(start);
    let mut selected = String::new();
    let mut actual_end = start.saturating_sub(1);
    let mut truncated = requested_end > end;
    for (index, line) in text.lines().enumerate().skip(start - 1) {
        let line_number = index + 1;
        if line_number > end {
            break;
        }
        let rendered = format!("{line_number}: {line}\n");
        if selected.len() + rendered.len() > MAX_READ_BYTES {
            truncated = true;
            break;
        }
        selected.push_str(&rendered);
        actual_end = line_number;
    }
    let relative = relative_display(&root, &file);
    Ok(ToolExecution {
        output: json!({
            "ok": true,
            "path": relative,
            "start_line": start,
            "end_line": actual_end,
            "truncated": truncated,
            "text": selected,
        })
        .to_string(),
        activity: format!("Read {relative}:{start}-{actual_end}"),
    })
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    path: Option<String>,
    max_results: Option<usize>,
}

fn search_text(arguments: &str, workspace: &Path) -> Result<ToolExecution> {
    let args: SearchArgs = serde_json::from_str(arguments)?;
    let query = args.query.trim();
    if query.is_empty() || query.len() > 256 {
        return Err(anyhow!("query must contain 1-256 bytes"));
    }
    let requested_path = args.path.as_deref().unwrap_or(".");
    let (root, target) = resolve_existing(workspace, requested_path)?;
    let limit = args
        .max_results
        .unwrap_or(MAX_SEARCH_RESULTS)
        .clamp(1, MAX_SEARCH_RESULTS);
    let mut files = collect_files(&target)?;
    files.sort();
    let mut matches = Vec::new();
    let mut files_checked = 0;
    for file in files.into_iter().take(MAX_SEARCH_FILES) {
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if metadata.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        files_checked += 1;
        for (index, line) in text.lines().enumerate() {
            if line.contains(query) {
                matches.push(json!({
                    "path": relative_display(&root, &file),
                    "line": index + 1,
                    "text": compact_line(line),
                }));
                if matches.len() == limit {
                    break;
                }
            }
        }
        if matches.len() == limit {
            break;
        }
    }
    Ok(ToolExecution {
        output: json!({
            "ok": true,
            "query": query,
            "path": requested_path,
            "matches": matches,
            "truncated": matches.len() == limit,
            "files_checked": files_checked,
        })
        .to_string(),
        activity: format!("Searched workspace for {query:?}"),
    })
}

#[derive(Deserialize)]
struct ListArgs {
    path: String,
}

fn list_directory(arguments: &str, workspace: &Path) -> Result<ToolExecution> {
    let args: ListArgs = serde_json::from_str(arguments)?;
    let (root, directory) = resolve_existing(workspace, &args.path)?;
    if !directory.is_dir() {
        return Err(anyhow!("path is not a directory"));
    }
    let mut entries = fs::read_dir(&directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if file_type.is_symlink() {
                return None;
            }
            Some(json!({
                "path": relative_display(&root, &entry.path()),
                "kind": if file_type.is_dir() { "directory" } else { "file" },
            }))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let truncated = entries.len() > MAX_DIRECTORY_ENTRIES;
    entries.truncate(MAX_DIRECTORY_ENTRIES);
    let relative = relative_display(&root, &directory);
    Ok(ToolExecution {
        output: json!({"ok": true, "path": relative, "entries": entries, "truncated": truncated})
            .to_string(),
        activity: format!("Listed {relative}"),
    })
}

fn resolve_existing(workspace: &Path, relative: &str) -> Result<(PathBuf, PathBuf)> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(anyhow!("path must stay inside the workspace"));
    }
    let root = workspace
        .canonicalize()
        .map_err(|_| anyhow!("workspace root is unavailable"))?;
    let target = root
        .join(relative)
        .canonicalize()
        .map_err(|_| anyhow!("workspace path does not exist"))?;
    if !target.starts_with(&root) {
        return Err(anyhow!("path escapes the workspace"));
    }
    Ok((root, target))
}

fn collect_files(target: &Path) -> Result<Vec<PathBuf>> {
    if target.is_file() {
        return Ok(vec![target.to_path_buf()]);
    }
    if !target.is_dir() {
        return Err(anyhow!("search path is not a file or directory"));
    }
    let mut files = Vec::new();
    let mut pending = vec![target.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        let mut directories = Vec::new();
        for entry in entries {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                if !ignored_directory(&entry.file_name().to_string_lossy()) {
                    directories.push(path);
                }
            } else if file_type.is_file() {
                files.push(path);
                if files.len() >= MAX_SEARCH_FILES {
                    return Ok(files);
                }
            }
        }
        // The stack is LIFO, so reverse to visit directories lexicographically.
        pending.extend(directories.into_iter().rev());
    }
    Ok(files)
}

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".hg" | ".svn" | ".angular" | ".nx" | "node_modules" | "target" | "dist" | "build"
    )
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn compact_line(line: &str) -> String {
    let compact = line.trim().chars().take(300).collect::<String>();
    if line.trim().chars().count() > 300 {
        format!("{compact}…")
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_is_line_bounded_and_workspace_relative() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
        let call = FunctionCall {
            call_id: "call_1".into(),
            name: "workspace_read_file".into(),
            arguments: r#"{"path":"a.rs","start_line":2,"end_line":3}"#.into(),
        };

        let result = execute(&call, workspace.path());

        assert!(result.output.contains("2: two"));
        assert!(result.output.contains("3: three"));
        assert!(
            !result
                .output
                .contains(workspace.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn oversize_file_read_returns_a_tool_error_instead_of_reading() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(
            workspace.path().join("huge.log"),
            vec![b'a'; MAX_READ_FILE_BYTES as usize + 1],
        )
        .unwrap();
        let call = FunctionCall {
            call_id: "call_1".into(),
            name: "workspace_read_file".into(),
            arguments: r#"{"path":"huge.log","start_line":null,"end_line":null}"#.into(),
        };

        let result = execute(&call, workspace.path());

        assert!(result.output.contains("\"ok\":false"));
        assert!(result.output.contains("read limit"));
    }

    #[test]
    fn rejects_parent_and_symlink_escapes() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "hidden").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("escape")).unwrap();

        assert!(resolve_existing(workspace.path(), "../secret").is_err());
        #[cfg(unix)]
        assert!(resolve_existing(workspace.path(), "escape/secret").is_err());
    }

    #[test]
    fn search_skips_dependency_and_build_directories() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("src")).unwrap();
        fs::create_dir(workspace.path().join("node_modules")).unwrap();
        fs::write(workspace.path().join("src/main.rs"), "needle\n").unwrap();
        fs::write(workspace.path().join("node_modules/noise.js"), "needle\n").unwrap();
        let call = FunctionCall {
            call_id: "call_1".into(),
            name: "workspace_search_text".into(),
            arguments: r#"{"query":"needle","path":null,"max_results":null}"#.into(),
        };

        let result = execute(&call, workspace.path());

        assert!(result.output.contains("src/main.rs"));
        assert!(!result.output.contains("noise.js"));
    }
}
