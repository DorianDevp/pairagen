use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use pair_protocol::{
    ContextArtifact, ContextArtifactKind, ContextBundle, ContextCandidateReport, ContextHint,
    ContextHintKind, ContextPolicy, ContextReport,
};

const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    "target",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "coverage",
    "__pycache__",
    ".cache",
    ".angular",
    ".nx",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".gradle",
    ".dart_tool",
    ".yarn",
    ".pnpm-store",
];

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs",
    "lua",
    "py",
    "js",
    "jsx",
    "ts",
    "tsx",
    "go",
    "java",
    "kt",
    "kts",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "cs",
    "rb",
    "php",
    "swift",
    "scala",
    "ex",
    "exs",
    "erl",
    "hrl",
    "fs",
    "fsx",
    "clj",
    "cljs",
    "vue",
    "svelte",
    "sql",
    "sh",
    "bash",
    "zsh",
    "fish",
    "vim",
    "nix",
    "toml",
    "yaml",
    "yml",
    "json",
    "md",
    "mdx",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "xml",
    "njk",
    "jinja",
    "jinja2",
    "hbs",
    "handlebars",
    "tera",
    "twig",
    "ejs",
    "mustache",
    "astro",
    "graphql",
    "gql",
];

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

#[derive(Default)]
pub struct ContextOptimizer {
    projects: HashMap<PathBuf, ProjectIndex>,
}

#[derive(Default)]
struct ProjectIndex {
    files: HashMap<PathBuf, IndexedFile>,
    last_refresh: Option<Instant>,
}

struct IndexedFile {
    modified_ns: u128,
    length: u64,
    lines: Vec<String>,
    lower_lines: Vec<String>,
    dependencies: Vec<String>,
}

#[derive(Clone)]
struct QueryTerm {
    value: String,
    weight: i32,
    source: &'static str,
    document_frequency: usize,
}

#[derive(Clone)]
struct QueryPath {
    normalized: String,
    basename: String,
}

struct RefreshStats {
    indexed_files: usize,
    hits: usize,
    misses: usize,
}

struct CandidateQuery<'a> {
    terms: &'a [QueryTerm],
    paths: &'a [QueryPath],
    current_file: Option<&'a Path>,
    primary_start_line: usize,
    primary_end_line: usize,
    current_dependencies: &'a [String],
    hints: &'a [ContextHint],
    diagnostics: &'a [pair_protocol::Diagnostic],
    root: &'a Path,
}

impl ContextOptimizer {
    pub fn optimize(
        &mut self,
        mut context: ContextBundle,
        prompt: &str,
        policy: &ContextPolicy,
    ) -> ContextBundle {
        context.artifacts.clear();
        context.report = None;

        if !policy.enabled {
            context.report = Some(ContextReport {
                enabled: false,
                primary_tokens: estimate_tokens(&context.buffer_text),
                used_tokens: estimate_tokens(&context.buffer_text),
                ..ContextReport::default()
            });
            return context;
        }

        let primary_truncated = trim_primary(&mut context, policy.primary_token_budget.max(1));
        let primary_tokens = estimate_tokens(&context.buffer_text);
        let root = canonical_or_original(&context.cwd);
        let project = self.projects.entry(root.clone()).or_default();
        let refresh = project.refresh(&root, policy);
        let mut terms = query_terms(&context, prompt);
        project.apply_document_frequency(&mut terms);
        let paths = query_paths(prompt);
        let current_dependencies = dependency_tokens(&context.buffer_text);
        let current_file = relative_file(&root, &context.file);
        let mut candidates = project.candidates(
            CandidateQuery {
                terms: &terms,
                paths: &paths,
                current_file: current_file.as_deref(),
                primary_start_line: context.buffer_start_line,
                primary_end_line: context
                    .buffer_start_line
                    .saturating_add(context.buffer_text.lines().count().saturating_sub(1)),
                current_dependencies: &current_dependencies,
                hints: &context.hints,
                diagnostics: &context.diagnostics,
                root: &root,
            },
            policy,
        );
        candidates.sort_by_key(|artifact| {
            (
                Reverse(artifact.score),
                artifact.estimated_tokens,
                artifact.file.clone(),
                artifact.start_line,
            )
        });

        let available = policy
            .total_token_budget
            .saturating_sub(policy.reserved_tokens)
            .saturating_sub(primary_tokens);
        let mut used = 0usize;
        let mut selected_keys = HashSet::new();

        for candidate in &candidates {
            if context.artifacts.len() >= policy.max_artifacts {
                break;
            }
            if candidate.estimated_tokens > available.saturating_sub(used) {
                continue;
            }
            if candidate.score < policy.min_artifact_score {
                continue;
            }

            used += candidate.estimated_tokens;
            selected_keys.insert(candidate_key(candidate));
            context.artifacts.push(candidate.clone());
        }

        let candidate_reports = candidates
            .iter()
            .take(48)
            .map(|candidate| ContextCandidateReport {
                file: candidate.file.clone(),
                start_line: candidate.start_line,
                kind: candidate.kind.clone(),
                reason: candidate.reason.clone(),
                estimated_tokens: candidate.estimated_tokens,
                score: candidate.score,
                selected: selected_keys.contains(&candidate_key(candidate)),
            })
            .collect::<Vec<_>>();

        context.report = Some(ContextReport {
            enabled: true,
            budget_tokens: policy.total_token_budget,
            used_tokens: primary_tokens + used,
            primary_tokens,
            artifact_tokens: used,
            indexed_files: refresh.indexed_files,
            candidate_count: candidates.len(),
            selected_count: context.artifacts.len(),
            primary_truncated,
            cache_hits: refresh.hits,
            cache_misses: refresh.misses,
            candidates: candidate_reports,
        });

        context
    }

    pub fn invalidate(&mut self, root: &Path, changed_files: &[PathBuf]) {
        let root = canonical_or_original(root);
        let Some(project) = self.projects.get_mut(&root) else {
            return;
        };

        for file in changed_files {
            if let Some(relative) = relative_file(&root, file) {
                project.files.remove(&relative);
            }
        }
        project.last_refresh = None;
    }
}

impl ProjectIndex {
    fn refresh(&mut self, root: &Path, policy: &ContextPolicy) -> RefreshStats {
        if self
            .last_refresh
            .is_some_and(|refresh| refresh.elapsed().as_millis() < policy.cache_ttl_ms as u128)
        {
            return RefreshStats {
                indexed_files: self.files.len(),
                hits: self.files.len(),
                misses: 0,
            };
        }
        let mut paths = Vec::new();
        collect_files(root, root, policy, &mut paths);
        paths.sort();
        paths.truncate(policy.max_scan_files);

        let mut seen = HashSet::new();
        let mut hits = 0;
        let mut misses = 0;

        for absolute in paths {
            let Ok(relative) = absolute.strip_prefix(root).map(Path::to_path_buf) else {
                continue;
            };
            let Ok(metadata) = fs::metadata(&absolute) else {
                continue;
            };
            let modified_ns = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let unchanged = self.files.get(&relative).is_some_and(|cached| {
                cached.modified_ns == modified_ns && cached.length == metadata.len()
            });

            seen.insert(relative.clone());
            if unchanged {
                hits += 1;
                continue;
            }

            let Ok(text) = fs::read_to_string(&absolute) else {
                continue;
            };
            if text.contains('\0') {
                continue;
            }
            let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
            let lower_lines = lines.iter().map(|line| line.to_lowercase()).collect();
            let dependencies = dependency_tokens(&text);
            self.files.insert(
                relative,
                IndexedFile {
                    modified_ns,
                    length: metadata.len(),
                    lines,
                    lower_lines,
                    dependencies,
                },
            );
            misses += 1;
        }

        self.files.retain(|path, _| seen.contains(path));
        self.last_refresh = Some(Instant::now());
        RefreshStats {
            indexed_files: self.files.len(),
            hits,
            misses,
        }
    }

    fn candidates(
        &self,
        query: CandidateQuery<'_>,
        policy: &ContextPolicy,
    ) -> Vec<ContextArtifact> {
        let CandidateQuery {
            terms,
            paths,
            current_file,
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
                if in_primary(line_index) {
                    continue;
                }
                let severity_bonus = match diagnostic.severity.as_str() {
                    "1" | "error" | "Error" => 30,
                    "2" | "warning" | "Warning" => 15,
                    _ => 0,
                };
                let score = 190 + severity_bonus;
                if best
                    .as_ref()
                    .is_none_or(|(_, best_score, _, _)| score > *best_score)
                {
                    best = Some((
                        line_index,
                        score,
                        ContextArtifactKind::Diagnostic,
                        format!("diagnostic: {}", diagnostic.message),
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

    fn apply_document_frequency(&self, terms: &mut [QueryTerm]) {
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

fn dependency_tokens(text: &str) -> Vec<String> {
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

fn collect_files(root: &Path, directory: &Path, policy: &ContextPolicy, files: &mut Vec<PathBuf>) {
    if files.len() >= policy.max_scan_files {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if files.len() >= policy.max_scan_files {
            break;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if excluded(relative, policy) {
            continue;
        }
        if file_type.is_dir() {
            collect_files(root, &path, policy, files);
        } else if file_type.is_file()
            && source_file(&path)
            && entry
                .metadata()
                .is_ok_and(|metadata| metadata.len() <= policy.max_file_bytes as u64)
        {
            files.push(path);
        }
    }
}

fn excluded(relative: &Path, policy: &ContextPolicy) -> bool {
    let components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| DEFAULT_EXCLUDED_DIRS.contains(component))
    {
        return true;
    }
    let normalized = relative.to_string_lossy().replace('\\', "/");
    policy.exclude.iter().any(|pattern| {
        let pattern = pattern.trim_matches('/');
        !pattern.is_empty()
            && (normalized == pattern
                || normalized.starts_with(&format!("{pattern}/"))
                || normalized.contains(&format!("/{pattern}/")))
    })
}

fn source_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if matches!(
        name,
        "Dockerfile" | "Makefile" | "Rakefile" | "Gemfile" | "Justfile" | "CMakeLists.txt"
    ) {
        return true;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SOURCE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

fn query_terms(context: &ContextBundle, prompt: &str) -> Vec<QueryTerm> {
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

fn query_paths(prompt: &str) -> Vec<QueryPath> {
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

fn trim_primary(context: &mut ContextBundle, budget: usize) -> bool {
    if estimate_tokens(&context.buffer_text) <= budget {
        return false;
    }
    let lines = context.buffer_text.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return false;
    }
    let cursor = context
        .cursor
        .line
        .saturating_sub(context.buffer_start_line)
        .min(lines.len() - 1);
    let mut start = cursor;
    let mut end = cursor + 1;

    loop {
        let before_distance = cursor.saturating_sub(start);
        let after_distance = end.saturating_sub(cursor + 1);
        let prefer_before = before_distance <= after_distance;
        let mut choices = Vec::with_capacity(2);
        if prefer_before {
            if start > 0 {
                choices.push((start - 1, end));
            }
            if end < lines.len() {
                choices.push((start, end + 1));
            }
        } else {
            if end < lines.len() {
                choices.push((start, end + 1));
            }
            if start > 0 {
                choices.push((start - 1, end));
            }
        }
        let Some((candidate_start, candidate_end)) = choices
            .into_iter()
            .find(|(left, right)| estimate_tokens(&lines[*left..*right].join("\n")) <= budget)
        else {
            break;
        };
        start = candidate_start;
        end = candidate_end;
    }

    context.buffer_text = lines[start..end].join("\n");
    context.buffer_start_line += start;
    true
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

fn relative_file(root: &Path, file: &Path) -> Option<PathBuf> {
    if file.is_absolute() {
        file.strip_prefix(root).ok().map(Path::to_path_buf)
    } else {
        Some(file.to_path_buf())
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn candidate_key(candidate: &ContextArtifact) -> (PathBuf, usize) {
    (candidate.file.clone(), candidate.start_line)
}

pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    (chars.div_ceil(4)).max(words).max(1)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pair_protocol::{ContextBundle, ContextHint, ContextHintKind, ContextPolicy, Cursor};

    use super::*;

    fn context(root: &Path, text: &str) -> ContextBundle {
        ContextBundle {
            cwd: root.to_path_buf(),
            file: PathBuf::from("src/main.rs"),
            cursor: Cursor { line: 2, column: 1 },
            selection: None,
            buffer_text: text.into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        }
    }

    #[test]
    fn ranks_definitions_and_respects_budget() {
        let root = std::env::temp_dir().join(format!("pair-context-{}", std::process::id()));
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
    fn truncates_primary_around_cursor() {
        let root = std::env::temp_dir();
        let mut input = context(&root, &vec!["long source line"; 100].join("\n"));
        input.cursor.line = 50;
        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            input,
            "unrelated",
            &ContextPolicy {
                primary_token_budget: 30,
                max_scan_files: 0,
                ..ContextPolicy::default()
            },
        );

        assert!(optimized.buffer_start_line < 50);
        assert!(optimized.buffer_start_line + optimized.buffer_text.lines().count() > 50);
        assert!(optimized.report.unwrap().primary_truncated);
    }

    #[test]
    fn follows_dependency_graph_without_prompt_name_overlap() {
        let root = std::env::temp_dir().join(format!("pair-context-graph-{}", std::process::id()));
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
        let root = std::env::temp_dir().join(format!("pair-context-lsp-{}", std::process::id()));
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
        let root =
            std::env::temp_dir().join(format!("pair-context-template-{}", std::process::id()));
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
            "pair-context-template-symbol-{}",
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
    fn minimum_score_discards_weak_textual_noise() {
        let root =
            std::env::temp_dir().join(format!("pair-context-threshold-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/noise.rs"), "// behavior\n").unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "fn main() {}\n"),
            "adjust behavior",
            &ContextPolicy {
                min_artifact_score: 100,
                ..ContextPolicy::default()
            },
        );

        assert!(optimized.artifacts.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn excludes_angular_build_cache_before_the_scan_limit() {
        let root =
            std::env::temp_dir().join(format!("pair-context-angular-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".angular/cache/21/babel-webpack")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join(".angular/cache/21/babel-webpack/generated.json"),
            r#"{"text":"preview_html preview_html preview_html"}"#,
        )
        .unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/preview.rs"), "pub fn preview_html() {}\n").unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "fn main() {}\n"),
            "Fix preview_html",
            &ContextPolicy {
                max_scan_files: 2,
                ..ContextPolicy::default()
            },
        );

        assert_eq!(optimized.report.as_ref().unwrap().indexed_files, 2);
        assert!(
            optimized
                .artifacts
                .iter()
                .all(|artifact| !artifact.file.starts_with(".angular"))
        );
        assert!(
            optimized
                .artifacts
                .iter()
                .any(|artifact| artifact.file == Path::new("src/preview.rs"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn selects_a_remote_definition_from_the_current_file() {
        let root =
            std::env::temp_dir().join(format!("pair-context-same-file-{}", std::process::id()));
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
}
