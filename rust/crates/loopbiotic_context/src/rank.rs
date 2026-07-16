use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use loopbiotic_protocol::{
    ContextArtifact, ContextArtifactKind, ContextBundle, ContextHint, ContextHintKind,
    ContextPolicy,
};

use crate::index::{IndexedFile, ProjectIndex, SOURCE_EXTENSIONS};
use crate::{estimate_tokens, relative_file};

const STOP_WORDS: &[&str] = &[
    "about",
    "adjust",
    "after",
    "agent",
    "async",
    "await",
    "before",
    "boolean",
    "change",
    "class",
    "code",
    "const",
    "could",
    "create",
    "current",
    "def",
    "enum",
    "explain",
    "file",
    "fix",
    "from",
    "func",
    "function",
    "have",
    "impl",
    "into",
    "just",
    "make",
    "more",
    "mut",
    "number",
    "only",
    "package",
    "please",
    "private",
    "protected",
    "pub",
    "public",
    "remove",
    "replace",
    "return",
    "review",
    "self",
    "should",
    "static",
    "string",
    "struct",
    "that",
    "the",
    "their",
    "then",
    "there",
    "this",
    "update",
    "void",
    "with",
    "would",
    "your",
    "błąd",
    "czemu",
    "dalej",
    "dodaję",
    "gdy",
    "jest",
    "jaki",
    "mam",
    "nie",
    "oraz",
    "resztę",
    "samo",
    "się",
    "styl",
    "żaden",
    "dodaj",
    "który",
    "która",
    "które",
    "let",
    "mamy",
    "może",
    "napraw",
    "niech",
    "plik",
    "przez",
    "tego",
    "teraz",
    "tylko",
    "usuń",
    "wyjaśnij",
    "zmień",
    "zrobić",
    "zmiana",
];

#[derive(Clone)]
pub(crate) struct QueryTerm {
    pub(crate) value: String,
    pub(crate) weight: i32,
    pub(crate) source: &'static str,
    pub(crate) document_frequency: usize,
}

#[derive(Clone)]
pub(crate) struct QueryPath {
    pub(crate) normalized: String,
    pub(crate) basename: String,
}

pub(crate) struct CandidateQuery<'a> {
    pub(crate) terms: &'a [QueryTerm],
    pub(crate) paths: &'a [QueryPath],
    pub(crate) current_file: Option<&'a Path>,
    pub(crate) cursor_line: usize,
    pub(crate) primary_start_line: usize,
    pub(crate) primary_end_line: usize,
    pub(crate) current_dependencies: &'a [String],
    pub(crate) hints: &'a [ContextHint],
    pub(crate) diagnostics: &'a [loopbiotic_protocol::Diagnostic],
    pub(crate) root: &'a Path,
}

impl ProjectIndex {
    pub(crate) fn candidates(
        &self,
        query: CandidateQuery<'_>,
        policy: &ContextPolicy,
    ) -> Vec<ContextArtifact> {
        let CandidateQuery {
            terms,
            paths,
            current_file,
            cursor_line,
            primary_start_line,
            primary_end_line,
            current_dependencies,
            hints,
            diagnostics,
            root,
        } = query;
        let mut candidates = Vec::new();

        for (path, file) in &self.files {
            if file.lines.is_empty() {
                continue;
            }

            let is_current_file = current_file.is_some_and(|current| current == path);
            let in_primary = |line_index: usize| {
                let line = line_index + 1;
                is_current_file && line >= primary_start_line && line <= primary_end_line
            };

            let path_lower = path.to_string_lossy().to_lowercase();
            let basename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_lowercase();
            let is_test = is_test_path(path);
            let mut best: Option<(usize, i32, ContextArtifactKind, String)> = None;
            let graph_distance = current_file
                .and_then(|current| self.graph_distance(current, current_dependencies, path));

            for query_path in paths {
                let exact_path = path_lower == query_path.normalized
                    || path_lower.ends_with(&format!("/{}", query_path.normalized));
                let exact_basename = basename == query_path.basename;
                if !exact_path && !exact_basename {
                    continue;
                }
                let line_index = first_query_line(file, terms)
                    .or_else(|| first_definition_line(file))
                    .unwrap_or(0);
                if in_primary(line_index) {
                    continue;
                }
                best = Some((
                    line_index,
                    if exact_path { 340 } else { 300 },
                    ContextArtifactKind::Reference,
                    format!("prompt path match: {}", query_path.normalized),
                ));
            }

            if let Some(distance) = graph_distance {
                let line_index = dependency_line(file, current_file)
                    .or_else(|| first_definition_line(file))
                    .unwrap_or(0);
                let score = if distance == 1 { 95 } else { 48 };
                best = Some((
                    line_index,
                    score,
                    ContextArtifactKind::Dependency,
                    format!("dependency graph distance {distance}"),
                ));
            }

            for hint in hints {
                let Some(hint_file) = relative_file(root, &hint.file) else {
                    continue;
                };
                if hint_file != *path || hint.line == 0 {
                    continue;
                }
                let line_index = hint.line.saturating_sub(1).min(file.lines.len() - 1);
                if in_primary(line_index) {
                    continue;
                }
                let (score, kind) = match hint.kind {
                    ContextHintKind::Definition | ContextHintKind::Declaration => {
                        (240, ContextArtifactKind::Definition)
                    }
                    ContextHintKind::TypeDefinition => (225, ContextArtifactKind::Definition),
                    ContextHintKind::Implementation => (210, ContextArtifactKind::Definition),
                    ContextHintKind::Reference => (165, ContextArtifactKind::Reference),
                };
                best = Some((
                    line_index,
                    score,
                    kind,
                    format!("{} {:?} result", hint.source, hint.kind).to_lowercase(),
                ));
            }

            for diagnostic in diagnostics {
                let Some(diagnostic_file) = relative_file(root, &diagnostic.file) else {
                    continue;
                };
                if diagnostic_file != *path || diagnostic.line == 0 {
                    continue;
                }
                let line_index = diagnostic.line.saturating_sub(1).min(file.lines.len() - 1);
                let severity_bonus = match diagnostic.severity.as_str() {
                    "1" | "error" | "Error" => 30,
                    "2" | "warning" | "Warning" => 15,
                    _ => 0,
                };
                let cursor_distance = diagnostic.line.abs_diff(cursor_line);
                let cursor_bonus = if is_current_file {
                    match cursor_distance {
                        0..=2 => 80,
                        3..=10 => 50,
                        11..=30 => 20,
                        _ => 0,
                    }
                } else {
                    0
                };
                let score = 190 + severity_bonus + cursor_bonus;
                if best
                    .as_ref()
                    .is_none_or(|(_, best_score, _, _)| score > *best_score)
                {
                    let proximity = if cursor_bonus > 0 {
                        format!(" near cursor ({} line(s) away)", cursor_distance)
                    } else {
                        String::new()
                    };
                    best = Some((
                        line_index,
                        score,
                        ContextArtifactKind::Diagnostic,
                        format!("diagnostic{proximity}: {}", diagnostic.message),
                    ));
                }
            }

            for (line_index, line) in file.lower_lines.iter().enumerate() {
                if in_primary(line_index) {
                    continue;
                }
                let mut score = 0;
                let mut matched = Vec::new();
                let mut strongest_source = "context";

                for term in terms {
                    if line_contains_term(line, &term.value) {
                        score += adjusted_term_weight(term, self.files.len());
                        matched.push(term.value.as_str());
                        strongest_source = term.source;
                        if is_definition_line(line, &term.value) {
                            score += definition_bonus(term, self.files.len());
                        }
                    }
                }

                if score == 0 {
                    continue;
                }
                if let Some(distance) = graph_distance {
                    score += if distance == 1 { 42 } else { 18 };
                }

                let definition = matched.iter().any(|term| is_definition_line(line, term));
                let dependency = looks_like_dependency(line);
                let kind = if definition {
                    ContextArtifactKind::Definition
                } else if is_test {
                    ContextArtifactKind::Test
                } else if dependency {
                    ContextArtifactKind::Dependency
                } else {
                    ContextArtifactKind::Reference
                };
                if is_test {
                    score += 8;
                }
                if dependency {
                    score += 6;
                }
                let names = matched.into_iter().take(3).collect::<Vec<_>>().join(", ");
                let reason = format!("{strongest_source} match: {names}");

                if best
                    .as_ref()
                    .is_none_or(|(_, best_score, _, _)| score > *best_score)
                {
                    best = Some((line_index, score, kind, reason));
                }
            }

            let Some((line_index, score, kind, reason)) = best else {
                continue;
            };
            let (start, end) = snippet_bounds(file.lines.len(), line_index, policy.snippet_lines);
            let text = file.lines[start..end].join("\n");
            let estimated_tokens = estimate_tokens(&text) + estimate_tokens(&path_lower) + 12;
            candidates.push(ContextArtifact {
                file: path.clone(),
                start_line: start + 1,
                end_line: end,
                kind,
                reason,
                text,
                estimated_tokens,
                score,
            });
        }

        candidates
    }

    pub(crate) fn apply_document_frequency(&self, terms: &mut [QueryTerm]) {
        for term in terms {
            term.document_frequency = self
                .files
                .iter()
                .filter(|(path, file)| {
                    path.to_string_lossy().to_lowercase().contains(&term.value)
                        || file
                            .lower_lines
                            .iter()
                            .any(|line| line_contains_term(line, &term.value))
                })
                .count();
        }
    }

    fn graph_distance(
        &self,
        current: &Path,
        current_dependencies: &[String],
        target: &Path,
    ) -> Option<usize> {
        let target_file = self.files.get(target)?;
        if dependencies_match(current_dependencies, target)
            || dependencies_match(&target_file.dependencies, current)
        {
            return Some(1);
        }

        for (middle_path, middle_file) in &self.files {
            if middle_path == current || middle_path == target {
                continue;
            }
            let current_middle = dependencies_match(current_dependencies, middle_path)
                || dependencies_match(&middle_file.dependencies, current);
            if current_middle
                && (dependencies_match(&middle_file.dependencies, target)
                    || dependencies_match(&target_file.dependencies, middle_path))
            {
                return Some(2);
            }
        }
        None
    }
}

pub(crate) fn dependency_tokens(text: &str) -> Vec<String> {
    let mut result = HashSet::new();
    for line in text
        .lines()
        .filter(|line| looks_like_dependency(&line.to_lowercase()))
    {
        let lower = line.to_lowercase();
        for token in identifiers(&lower) {
            if !matches!(
                token.as_str(),
                "use" | "mod" | "pub" | "import" | "from" | "require" | "include"
            ) {
                result.insert(token);
            }
        }
        for quoted in quoted_values(&lower) {
            result.insert(quoted.replace(['.', ':', '\\'], "/"));
        }
    }
    let mut result = result.into_iter().collect::<Vec<_>>();
    result.sort();
    result
}

fn quoted_values(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    for quote in ['\'', '"'] {
        let mut rest = text;
        while let Some(start) = rest.find(quote) {
            rest = &rest[start + quote.len_utf8()..];
            let Some(end) = rest.find(quote) else {
                break;
            };
            if end > 0 {
                values.push(rest[..end].to_string());
            }
            rest = &rest[end + quote.len_utf8()..];
        }
    }
    values
}

fn dependencies_match(dependencies: &[String], path: &Path) -> bool {
    let normalized = path.to_string_lossy().to_lowercase().replace('\\', "/");
    let without_extension = normalized
        .rsplit_once('.')
        .map(|(value, _)| value)
        .unwrap_or(&normalized);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("")
        .to_lowercase();
    dependencies.iter().any(|dependency| {
        dependency == &stem
            || without_extension.ends_with(dependency)
            || without_extension.ends_with(&format!("/{dependency}"))
            || dependency.ends_with(&format!("/{stem}"))
    })
}

fn dependency_line(file: &IndexedFile, target: Option<&Path>) -> Option<usize> {
    let target = target?;
    let stem = target.file_stem()?.to_str()?.to_lowercase();
    file.lower_lines
        .iter()
        .position(|line| looks_like_dependency(line) && line.contains(&stem))
}

fn first_definition_line(file: &IndexedFile) -> Option<usize> {
    file.lower_lines.iter().position(|line| {
        let trimmed = line.trim_start();
        [
            "pub ",
            "fn ",
            "struct ",
            "enum ",
            "class ",
            "def ",
            "function ",
            "interface ",
            "type ",
            "func ",
        ]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    })
}

pub(crate) fn query_terms(context: &ContextBundle, prompt: &str) -> Vec<QueryTerm> {
    let mut weights = HashMap::<String, (i32, &'static str)>::new();
    add_terms(&mut weights, prompt, 14, "prompt");
    if let Some(selection) = &context.selection {
        add_terms(&mut weights, &selection.text, 9, "selection");
    }
    let cursor_offset = context
        .cursor
        .line
        .saturating_sub(context.buffer_start_line);
    let lines = context.buffer_text.lines().collect::<Vec<_>>();
    let start = cursor_offset.saturating_sub(2);
    let end = (cursor_offset + 3).min(lines.len());
    if start < end {
        add_terms(&mut weights, &lines[start..end].join("\n"), 5, "cursor");
    }

    let mut terms = weights
        .into_iter()
        .map(|(value, (weight, source))| QueryTerm {
            value,
            weight,
            source,
            document_frequency: 0,
        })
        .collect::<Vec<_>>();
    terms.sort_by_key(|term| (Reverse(term.weight), Reverse(term.value.len())));
    terms.truncate(32);
    terms
}

pub(crate) fn query_paths(prompt: &str) -> Vec<QueryPath> {
    let mut paths = HashSet::new();
    for raw in prompt.split_whitespace() {
        let normalized = raw
            .trim_matches(|character: char| {
                matches!(
                    character,
                    '`' | '\''
                        | '"'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | ','
                        | ';'
                        | ':'
                        | '?'
                        | '!'
                        | '.'
                )
            })
            .replace('\\', "/")
            .to_lowercase();
        let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
        let has_known_extension = basename
            .rsplit_once('.')
            .is_some_and(|(_, extension)| SOURCE_EXTENSIONS.contains(&extension));
        if has_known_extension {
            paths.insert(normalized);
        }
    }

    let mut paths = paths
        .into_iter()
        .map(|normalized| QueryPath {
            basename: normalized
                .rsplit('/')
                .next()
                .unwrap_or(&normalized)
                .to_string(),
            normalized,
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| left.normalized.cmp(&right.normalized));
    paths
}

fn adjusted_term_weight(term: &QueryTerm, file_count: usize) -> i32 {
    let frequency = term.document_frequency.max(1);
    let compound_prompt_bonus =
        if term.source == "prompt" && (term.value.contains('_') || term.value.contains('-')) {
            20
        } else {
            0
        };
    let weight = if frequency == 1 {
        term.weight + 18
    } else if frequency <= 3 {
        term.weight + 12
    } else if frequency <= 8 {
        term.weight + 5
    } else if frequency * 4 > file_count.max(1) {
        (term.weight / 3).max(2)
    } else {
        term.weight
    };
    weight + compound_prompt_bonus
}

fn definition_bonus(term: &QueryTerm, file_count: usize) -> i32 {
    let rare_limit = (file_count / 10).max(3);
    if term.value.len() >= 5 && term.document_frequency <= rare_limit {
        55
    } else {
        12
    }
}

fn first_query_line(file: &IndexedFile, terms: &[QueryTerm]) -> Option<usize> {
    file.lower_lines.iter().position(|line| {
        terms
            .iter()
            .any(|term| line_contains_term(line, &term.value))
    })
}

fn add_terms(
    terms: &mut HashMap<String, (i32, &'static str)>,
    text: &str,
    weight: i32,
    source: &'static str,
) {
    for term in identifiers(text) {
        let entry = terms.entry(term).or_insert((weight, source));
        if weight > entry.0 {
            *entry = (weight, source);
        }
    }
}

fn identifiers(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            current.extend(character.to_lowercase());
        } else {
            push_identifier(&mut result, &mut current);
        }
    }
    push_identifier(&mut result, &mut current);
    result
}

fn push_identifier(result: &mut Vec<String>, current: &mut String) {
    if current.len() >= 3
        && !current.chars().all(|character| character.is_ascii_digit())
        && !STOP_WORDS.contains(&current.as_str())
    {
        result.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn snippet_bounds(total: usize, center: usize, requested: usize) -> (usize, usize) {
    let count = requested.max(3).min(total);
    let before = count / 2;
    let mut start = center.saturating_sub(before);
    let mut end = (start + count).min(total);
    start = end.saturating_sub(count);
    end = (start + count).min(total);
    (start, end)
}

fn is_definition_line(line: &str, term: &str) -> bool {
    [
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "type ",
        "class ",
        "def ",
        "function ",
        "interface ",
        "const ",
        "let ",
        "var ",
        "func ",
        "module ",
        "pub ",
    ]
    .iter()
    .any(|prefix| line.contains(&format!("{prefix}{term}")))
}

fn looks_like_dependency(line: &str) -> bool {
    let trimmed = line.trim_start();
    [
        "use ", "mod ", "import ", "from ", "require(", "require ", "include ", "#include",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
}

fn line_contains_term(line: &str, term: &str) -> bool {
    line.match_indices(term).any(|(start, _)| {
        let before = line[..start].chars().next_back();
        let after = line[start + term.len()..].chars().next();
        let css_custom_property = line[..start].ends_with("--");
        (css_custom_property || before.is_none_or(|character| !is_identifier_character(character)))
            && after.is_none_or(|character| !is_identifier_character(character))
    })
}

fn is_identifier_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '-'
}

fn is_test_path(path: &Path) -> bool {
    let value = path.to_string_lossy().to_lowercase();
    value.contains("/test")
        || value.contains("tests/")
        || value.contains("_test.")
        || value.contains(".test.")
        || value.contains(".spec.")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use loopbiotic_protocol::{
        ContextArtifactKind, ContextHint, ContextHintKind, ContextPolicy, Diagnostic,
    };

    use crate::ContextOptimizer;
    use crate::test_support::context;

    #[test]
    fn ranks_definitions_and_respects_budget() {
        let root = std::env::temp_dir().join(format!("loopbiotic-context-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    register_user();\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/users.rs"),
            "pub fn register_user() {\n    validate_email();\n}\n",
        )
        .unwrap();
        fs::write(root.join("src/noise.rs"), "pub fn unrelated() {}\n").unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "fn main() {\n    register_user();\n}\n"),
            "Fix register_user validation",
            &ContextPolicy {
                total_token_budget: 1_200,
                reserved_tokens: 200,
                ..ContextPolicy::default()
            },
        );

        assert_eq!(optimized.artifacts[0].file, PathBuf::from("src/users.rs"));
        assert_eq!(optimized.artifacts[0].kind, ContextArtifactKind::Definition);
        assert!(optimized.report.unwrap().used_tokens <= 1_000);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn follows_dependency_graph_without_prompt_name_overlap() {
        let root =
            std::env::temp_dir().join(format!("loopbiotic-context-graph-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "mod users;\nfn main() {}\n").unwrap();
        fs::write(root.join("src/users.rs"), "pub fn register() {}\n").unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "mod users;\nfn main() {}\n"),
            "adjust the behavior",
            &ContextPolicy::default(),
        );

        let users = optimized
            .artifacts
            .iter()
            .find(|artifact| artifact.file == Path::new("src/users.rs"))
            .expect("dependency should be selected");
        assert_eq!(users.kind, ContextArtifactKind::Dependency);
        assert!(users.reason.contains("dependency graph"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lsp_hint_outranks_textual_candidates_and_cache_is_reused() {
        let root =
            std::env::temp_dir().join(format!("loopbiotic-context-lsp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.go"), "package main\nfunc main() {}\n").unwrap();
        fs::write(
            root.join("src/service.go"),
            "package main\nfunc ResolveTarget() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/noise.go"),
            "package main\nfunc ResolveTargetNoise() {}\n",
        )
        .unwrap();

        let mut input = context(&root, "package main\nfunc main() {}\n");
        input.file = PathBuf::from("src/main.go");
        input.hints.push(ContextHint {
            file: PathBuf::from("src/service.go"),
            line: 2,
            column: 1,
            kind: ContextHintKind::Definition,
            source: "gopls".into(),
        });
        let mut optimizer = ContextOptimizer::default();
        let first = optimizer.optimize(input.clone(), "ResolveTarget", &ContextPolicy::default());
        let second = optimizer.optimize(input, "ResolveTarget", &ContextPolicy::default());

        assert_eq!(first.artifacts[0].file, PathBuf::from("src/service.go"));
        assert!(first.artifacts[0].reason.contains("gopls"));
        let report = second.report.unwrap();
        assert!(report.cache_hits >= 3);
        assert_eq!(report.cache_misses, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exact_template_path_outranks_common_html_symbols() {
        let root = std::env::temp_dir().join(format!(
            "loopbiotic-context-template-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("templates")).unwrap();
        fs::write(
            root.join("src/main.rs"),
            "fn html() -> String { String::new() }\nfn render_html() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/noise.rs"),
            "pub fn html() {}\npub fn another_html() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("templates/layout_editor.html"),
            "<section>{{ block.preview_html|safe }}</section>\n",
        )
        .unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "fn html() -> String { String::new() }\n"),
            "Update templates/layout_editor.html to render the concrete preview",
            &ContextPolicy::default(),
        );

        assert_eq!(
            optimized.artifacts[0].file,
            PathBuf::from("templates/layout_editor.html")
        );
        assert!(optimized.artifacts[0].reason.contains("prompt path match"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rare_compound_prompt_symbol_selects_its_template_without_a_path() {
        let root = std::env::temp_dir().join(format!(
            "loopbiotic-context-template-symbol-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("templates/admin")).unwrap();
        fs::write(root.join("src/main.rs"), "fn html() {}\n").unwrap();
        fs::write(
            root.join("templates/admin/layout_editor.html"),
            "<section>{{ block.preview_html|safe }}</section>\n",
        )
        .unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "fn html() {}\n"),
            "Replace preview_html with concrete structs for Askama templates",
            &ContextPolicy::default(),
        );

        assert!(
            optimized.artifacts.iter().any(|artifact| {
                artifact.file == Path::new("templates/admin/layout_editor.html")
            })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn selects_a_remote_definition_from_the_current_file() {
        let root = std::env::temp_dir().join(format!(
            "loopbiotic-context-same-file-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        let mut lines = vec!["// filler"; 80];
        lines[3] = "--text-vw-h3--font-weight: 800;";
        lines[70] = "h3 { @apply text-vw-h3 text-vw-h3--font-weight; }";
        fs::write(root.join("src/main.scss"), lines.join("\n")).unwrap();

        let mut input = context(&root, "h3 { @apply text-vw-h3 text-vw-h3--font-weight; }");
        input.file = PathBuf::from("src/main.scss");
        input.buffer_start_line = 71;
        input.cursor.line = 71;
        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            input,
            "Why does text-vw-h3--font-weight fail?",
            &ContextPolicy::default(),
        );

        let remote = optimized
            .artifacts
            .iter()
            .find(|artifact| artifact.file == Path::new("src/main.scss"))
            .expect("remote definition from the current file should be selected");
        assert!(remote.start_line < 20);
        assert!(remote.text.contains("--text-vw-h3--font-weight"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cursor_local_error_outranks_distant_deprecation_in_primary_buffer() {
        let root = std::env::temp_dir().join(format!(
            "loopbiotic-context-local-diagnostic-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("static")).unwrap();
        let mut lines = vec!["// filler"; 300];
        lines[164] = "document.write(html);";
        lines[258] = "body = new URLSearchParams(formData);";
        fs::write(root.join("static/admin.js"), lines.join("\n")).unwrap();

        let mut input = context(&root, &lines[234..270].join("\n"));
        input.file = PathBuf::from("static/admin.js");
        input.buffer_start_line = 235;
        input.cursor.line = 259;
        input.cursor.column = 16;
        input.diagnostics = vec![
            Diagnostic {
                file: PathBuf::from("static/admin.js"),
                line: 259,
                column: 5,
                severity: "1".into(),
                message: "Type 'URLSearchParams' is not assignable to type 'FormData'.".into(),
            },
            Diagnostic {
                file: PathBuf::from("static/admin.js"),
                line: 165,
                column: 16,
                severity: "4".into(),
                message: "The signature of 'document.write' is deprecated.".into(),
            },
        ];

        let mut optimizer = ContextOptimizer::default();
        let optimized =
            optimizer.optimize(input, "What's wrong with it?", &ContextPolicy::default());

        let diagnostic = optimized
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == ContextArtifactKind::Diagnostic)
            .expect("cursor-local diagnostic should remain explicit context");
        assert_eq!(diagnostic.start_line, 254);
        assert!(diagnostic.reason.contains("near cursor"));
        assert!(diagnostic.reason.contains("URLSearchParams"));
        assert!(!diagnostic.reason.contains("document.write"));
        assert_eq!(diagnostic.score, 300);
        let report = optimized.report.unwrap();
        assert!(
            report.candidates.iter().any(|candidate| {
                candidate.selected
                    && candidate.kind == ContextArtifactKind::Diagnostic
                    && candidate.reason.contains("URLSearchParams")
            }),
            "telemetry should report the local error as delivered"
        );
        let _ = fs::remove_dir_all(root);
    }
}
