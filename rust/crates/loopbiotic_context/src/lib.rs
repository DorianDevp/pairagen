mod index;
pub mod project;
mod rank;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use loopbiotic_protocol::{
    ContextArtifact, ContextBundle, ContextCandidateReport, ContextPolicy, ContextReport,
};

use index::ProjectIndex;
use rank::{CandidateQuery, dependency_tokens, query_paths, query_terms};

#[derive(Default)]
pub struct ContextOptimizer {
    projects: HashMap<PathBuf, ProjectIndex>,
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
                cursor_line: context.cursor.line,
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

pub(crate) fn relative_file(root: &Path, file: &Path) -> Option<PathBuf> {
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
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    use loopbiotic_protocol::{ContextBundle, Cursor};

    pub(crate) fn context(root: &Path, text: &str) -> ContextBundle {
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
            call_hierarchy: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use loopbiotic_protocol::ContextPolicy;

    use super::*;
    use crate::test_support::context;

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
    fn minimum_score_discards_weak_textual_noise() {
        let root = std::env::temp_dir().join(format!(
            "loopbiotic-context-threshold-{}",
            std::process::id()
        ));
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
}
