use std::collections::BTreeSet;

use anyhow::{Result, anyhow};
use pair_protocol::{
    Card, ContextBundle, FilePatch, MAX_CHANGED_LINES, MAX_HUNKS_PER_PATCH, MAX_PATCH_FILES,
};

use crate::unified_diff::{DiffLine, UnifiedDiff};

pub struct PatchValidator;
pub struct PatchNormalizer;
pub struct PatchCoherence;

impl PatchCoherence {
    pub fn annotate(card: &mut Card) {
        let Card::Patch(card) = card else {
            return;
        };
        let description = format!(
            "{} {} {}",
            card.title,
            card.explanation,
            card.patches
                .iter()
                .map(|patch| patch.explanation.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
        .to_ascii_lowercase();
        if !description.contains("renam") {
            return;
        }

        for patch in &card.patches {
            let Ok(diff) = UnifiedDiff::parse(&patch.diff) else {
                continue;
            };
            for hunk in &diff.hunks {
                let removed =
                    identifiers_for_lines(&hunk.lines, |line| matches!(line, DiffLine::Remove(_)));
                let added =
                    identifiers_for_lines(&hunk.lines, |line| matches!(line, DiffLine::Add(_)));
                let context =
                    identifiers_for_lines(&hunk.lines, |line| matches!(line, DiffLine::Context(_)));
                let old = removed.difference(&added).collect::<Vec<_>>();
                let new = added.difference(&removed).collect::<Vec<_>>();

                if let ([old], [new]) = (old.as_slice(), new.as_slice())
                    && context.contains(*old)
                {
                    card.warnings.push(format!(
                        "Possible incomplete rename: {old} was changed to {new}, but unchanged hunk context still references {old}."
                    ));
                }
            }
        }
    }
}

impl PatchNormalizer {
    pub fn normalize_card(card: &mut Card, context: &ContextBundle) -> Result<()> {
        let Card::Patch(card) = card else {
            return Ok(());
        };
        let source = context.buffer_text.lines().collect::<Vec<_>>();

        for patch in &mut card.patches {
            let mut diff = UnifiedDiff::parse(&patch.diff)?;
            for hunk in &mut diff.hunks {
                let expected = hunk
                    .lines
                    .iter()
                    .filter_map(|line| match line {
                        DiffLine::Context(text) | DiffLine::Remove(text) => Some(text.as_str()),
                        DiffLine::Add(_) => None,
                    })
                    .collect::<Vec<_>>();
                if expected.is_empty() {
                    return Err(anyhow!("patch hunk has no source context"));
                }

                let declared = hunk.old_start.checked_sub(context.buffer_start_line);
                let start = if declared.is_some_and(|start| matches_at(&source, start, &expected)) {
                    declared.unwrap()
                } else {
                    let matches = (0..=source.len().saturating_sub(expected.len()))
                        .filter(|start| matches_at(&source, *start, &expected))
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [start] => *start,
                        [] => {
                            return Err(anyhow!(
                                "patch context was not found in the supplied buffer"
                            ));
                        }
                        _ => {
                            return Err(anyhow!(
                                "patch context is ambiguous in the supplied buffer"
                            ));
                        }
                    }
                };

                let corrected_old_start = context.buffer_start_line + start;
                let delta = hunk.new_start as isize - hunk.old_start as isize;
                let corrected_new_start = corrected_old_start
                    .checked_add_signed(delta)
                    .ok_or_else(|| anyhow!("corrected patch coordinates are outside the file"))?;
                if corrected_new_start == 0 {
                    return Err(anyhow!("corrected patch coordinates must start at 1"));
                }

                hunk.old_start = corrected_old_start;
                hunk.new_start = corrected_new_start;
            }
            patch.diff = render_diff(&diff);
        }

        Ok(())
    }
}

impl PatchValidator {
    pub fn validate_card(card: &Card) -> Result<()> {
        Self::validate_card_with_limits(
            card,
            MAX_PATCH_FILES,
            MAX_HUNKS_PER_PATCH,
            MAX_CHANGED_LINES,
        )
    }

    pub fn validate_card_with_limits(
        card: &Card,
        max_patch_files: usize,
        max_hunks_per_patch: usize,
        max_changed_lines: usize,
    ) -> Result<()> {
        let Card::Patch(card) = card else {
            return Ok(());
        };

        if card.patches.is_empty() {
            return Err(anyhow!("patch card has no patches"));
        }
        if card.patches.len() > max_patch_files {
            return Err(anyhow!(
                "patch card changes {} files; maximum is {max_patch_files}",
                card.patches.len(),
            ));
        }

        for patch in &card.patches {
            Self::validate_file_patch_with_limits(patch, max_hunks_per_patch, max_changed_lines)?;
        }

        Ok(())
    }

    pub fn validate_file_patch(patch: &FilePatch) -> Result<()> {
        Self::validate_file_patch_with_limits(patch, MAX_HUNKS_PER_PATCH, MAX_CHANGED_LINES)
    }

    fn validate_file_patch_with_limits(
        patch: &FilePatch,
        max_hunks_per_patch: usize,
        max_changed_lines: usize,
    ) -> Result<()> {
        if patch.id.trim().is_empty() {
            return Err(anyhow!("patch id is empty"));
        }

        if patch.file.as_os_str().is_empty() {
            return Err(anyhow!("patch file is empty"));
        }

        if patch.file.is_absolute() {
            return Err(anyhow!("patch file must be relative"));
        }

        let diff = UnifiedDiff::parse(&patch.diff)?;
        if diff.hunks.len() > max_hunks_per_patch {
            return Err(anyhow!(
                "patch has {} hunks; maximum is {max_hunks_per_patch}",
                diff.hunks.len(),
            ));
        }

        for hunk in diff.hunks {
            validate_hunk_counts(&hunk, max_changed_lines)?;
        }

        Ok(())
    }

    pub fn validate_card_against_context(card: &Card, context: &ContextBundle) -> Result<()> {
        let Card::Patch(card) = card else {
            return Ok(());
        };
        let source = context.buffer_text.lines().collect::<Vec<_>>();

        for patch in &card.patches {
            let diff = UnifiedDiff::parse(&patch.diff)?;
            for hunk in &diff.hunks {
                let start = hunk
                    .old_start
                    .checked_sub(context.buffer_start_line)
                    .ok_or_else(|| {
                        anyhow!("patch hunk starts before the supplied buffer excerpt")
                    })?;
                let expected = hunk.lines.iter().filter_map(|line| match line {
                    DiffLine::Context(text) | DiffLine::Remove(text) => Some(text.as_str()),
                    DiffLine::Add(_) => None,
                });

                for (offset, expected) in expected.enumerate() {
                    let line = context.buffer_start_line + start + offset;
                    let actual = source.get(start + offset).copied().ok_or_else(|| {
                        anyhow!(
                            "patch source context at line {line} is outside the supplied buffer"
                        )
                    })?;
                    if actual != expected {
                        return Err(anyhow!(
                            "patch source context mismatch at line {line}: expected {expected:?}, got {actual:?}"
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

fn validate_hunk_counts(hunk: &crate::Hunk, max_changed_lines: usize) -> Result<()> {
    let old_count = hunk
        .lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Context(_) | DiffLine::Remove(_)))
        .count();
    let new_count = hunk
        .lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Context(_) | DiffLine::Add(_)))
        .count();

    if old_count == 0 {
        return Err(anyhow!("hunk has no source context"));
    }
    if old_count != hunk.old_len || new_count != hunk.new_len {
        return Err(anyhow!("hunk header counts do not match its lines"));
    }

    let changed_lines = hunk
        .lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Remove(_) | DiffLine::Add(_)))
        .count();
    if changed_lines > max_changed_lines {
        return Err(anyhow!(
            "hunk changes {changed_lines} lines; maximum is {max_changed_lines}"
        ));
    }

    Ok(())
}

fn matches_at(source: &[&str], start: usize, expected: &[&str]) -> bool {
    expected
        .iter()
        .enumerate()
        .all(|(offset, line)| source.get(start + offset).copied() == Some(*line))
}

fn render_diff(diff: &UnifiedDiff) -> String {
    diff.render()
}

fn identifiers_for_lines(
    lines: &[DiffLine],
    include: impl Fn(&DiffLine) -> bool,
) -> BTreeSet<String> {
    lines
        .iter()
        .filter(|line| include(line))
        .flat_map(|line| match line {
            DiffLine::Context(text) | DiffLine::Remove(text) | DiffLine::Add(text) => {
                identifiers(text)
            }
        })
        .collect()
}

fn identifiers(text: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut current = String::new();

    for character in text.chars().chain(std::iter::once(' ')) {
        if character == '_' || character.is_ascii_alphanumeric() {
            current.push(character);
        } else if !current.is_empty() {
            if current
                .chars()
                .next()
                .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            {
                identifiers.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }

    identifiers
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pair_protocol::FilePatch;

    use super::*;

    #[test]
    fn rejects_absolute_file() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("/tmp/work.ts"),
            diff: "@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
            explanation: String::new(),
        };
        let error = PatchValidator::validate_file_patch(&patch).unwrap_err();

        assert!(error.to_string().contains("relative"));
    }

    #[test]
    fn accepts_simple_patch() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
            explanation: String::new(),
        };

        PatchValidator::validate_file_patch(&patch).unwrap();
    }

    #[test]
    fn rejects_more_than_one_hunk() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,1 +1,1 @@\n-old\n+new\n@@ -4,1 +4,1 @@\n-old 2\n+new 2\n".into(),
            explanation: String::new(),
        };

        let error = PatchValidator::validate_file_patch(&patch).unwrap_err();

        assert!(error.to_string().contains("maximum is 1"));
    }

    #[test]
    fn goal_batch_accepts_a_formatting_hunk_over_the_review_limit() {
        let mut diff = "@@ -1,20 +1,20 @@\n".to_string();
        for index in 0..20 {
            diff.push_str(&format!("-line {index}\n"));
        }
        for index in 0..20 {
            diff.push_str(&format!("+    line {index}\n"));
        }
        let card = Card::Patch(pair_protocol::PatchCard {
            id: "c_format".into(),
            title: "Format".into(),
            explanation: "Indent the block.".into(),
            warnings: vec![],
            goal_complete: true,
            patches: vec![FilePatch {
                id: "p_format".into(),
                file: PathBuf::from("templates/view.html"),
                diff,
                explanation: "Indent the block.".into(),
            }],
            actions: vec![pair_protocol::Action::Apply],
        });

        assert!(PatchValidator::validate_card(&card).is_err());
        PatchValidator::validate_card_with_limits(
            &card,
            1,
            pair_protocol::MAX_GOAL_HUNKS_PER_PATCH,
            pair_protocol::MAX_GOAL_CHANGED_LINES,
        )
        .unwrap();
    }

    #[test]
    fn rejects_incorrect_hunk_header_counts() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,2 +1,2 @@\n-old\n+new\n".into(),
            explanation: String::new(),
        };

        let error = PatchValidator::validate_file_patch(&patch).unwrap_err();

        assert!(error.to_string().contains("header counts"));
    }

    #[test]
    fn rejects_hunk_without_source_context() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,0 +1,1 @@\n+new\n".into(),
            explanation: "Insert a line.".into(),
        };

        let error = PatchValidator::validate_file_patch(&patch).unwrap_err();

        assert!(error.to_string().contains("no source context"));
    }

    #[test]
    fn validates_hunk_against_buffer_file_coordinates() {
        let card = Card::Patch(pair_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -51,1 +51,1 @@\n-old\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            actions: vec![pair_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: pair_protocol::Cursor {
                line: 51,
                column: 1,
            },
            selection: None,
            buffer_text: "before\nold\nafter".into(),
            buffer_start_line: 50,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        };

        PatchValidator::validate_card_against_context(&card, &context).unwrap();
    }

    #[test]
    fn rejects_hunk_that_does_not_match_buffer_context() {
        let card = Card::Patch(pair_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -2,1 +2,1 @@\n-stale\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            actions: vec![pair_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: pair_protocol::Cursor { line: 2, column: 1 },
            selection: None,
            buffer_text: "before\ncurrent\nafter".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        };

        let error = PatchValidator::validate_card_against_context(&card, &context).unwrap_err();

        assert!(error.to_string().contains("mismatch at line 2"));
    }

    #[test]
    fn normalizes_uniquely_relocated_hunk_coordinates() {
        let mut card = Card::Patch(pair_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -52,2 +52,2 @@\n marker\n-old\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            actions: vec![pair_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: pair_protocol::Cursor {
                line: 51,
                column: 1,
            },
            selection: None,
            buffer_text: "marker\nold\nafter".into(),
            buffer_start_line: 50,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        };

        PatchNormalizer::normalize_card(&mut card, &context).unwrap();
        PatchValidator::validate_card_against_context(&card, &context).unwrap();

        let Card::Patch(card) = card else {
            unreachable!();
        };
        assert!(card.patches[0].diff.starts_with("@@ -50,2 +50,2 @@"));
    }

    #[test]
    fn refuses_to_relocate_ambiguous_hunk_context() {
        let mut card = Card::Patch(pair_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -9,1 +9,1 @@\n-old\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            actions: vec![pair_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: pair_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "old\nbetween\nold".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
        };

        let error = PatchNormalizer::normalize_card(&mut card, &context).unwrap_err();

        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn warns_about_an_incomplete_local_rename() {
        let mut card = Card::Patch(pair_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename response binding".into(),
            explanation: "Rename response to rpc_response.".into(),
            warnings: vec![],
            goal_complete: false,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.rs"),
                diff: "@@ -1,2 +1,2 @@\n-let response = call();\n+let rpc_response = call();\n use_value(response);\n"
                    .into(),
                explanation: "Rename the local binding.".into(),
            }],
            actions: vec![pair_protocol::Action::Apply],
        });

        PatchCoherence::annotate(&mut card);

        let Card::Patch(card) = card else {
            unreachable!();
        };
        assert_eq!(card.warnings.len(), 1);
        assert!(card.warnings[0].contains("response"));
        assert!(card.warnings[0].contains("rpc_response"));
    }

    #[test]
    fn complete_local_rename_has_no_warning() {
        let mut card = Card::Patch(pair_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename response binding".into(),
            explanation: "Rename response to rpc_response.".into(),
            warnings: vec![],
            goal_complete: false,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.rs"),
                diff: "@@ -1,2 +1,2 @@\n-let response = call();\n-use_value(response);\n+let rpc_response = call();\n+use_value(rpc_response);\n"
                    .into(),
                explanation: "Rename the binding and its use.".into(),
            }],
            actions: vec![pair_protocol::Action::Apply],
        });

        PatchCoherence::annotate(&mut card);

        let Card::Patch(card) = card else {
            unreachable!();
        };
        assert!(card.warnings.is_empty());
    }
}
