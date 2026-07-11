use anyhow::{Result, anyhow};
use pair_protocol::{
    Card, ContextBundle, FilePatch, MAX_CHANGED_LINES, MAX_HUNKS_PER_PATCH, MAX_PATCH_FILES,
};

use crate::unified_diff::{DiffLine, UnifiedDiff};

pub struct PatchValidator;
pub struct PatchNormalizer;

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
        let Card::Patch(card) = card else {
            return Ok(());
        };

        if card.patches.is_empty() {
            return Err(anyhow!("patch card has no patches"));
        }
        if card.patches.len() > MAX_PATCH_FILES {
            return Err(anyhow!(
                "patch card changes {} files; maximum is {MAX_PATCH_FILES}",
                card.patches.len()
            ));
        }

        for patch in &card.patches {
            Self::validate_file_patch(patch)?;
        }

        Ok(())
    }

    pub fn validate_file_patch(patch: &FilePatch) -> Result<()> {
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
        if diff.hunks.len() > MAX_HUNKS_PER_PATCH {
            return Err(anyhow!(
                "patch has {} hunks; maximum is {MAX_HUNKS_PER_PATCH}",
                diff.hunks.len()
            ));
        }

        for hunk in diff.hunks {
            validate_hunk_counts(&hunk)?;
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

fn validate_hunk_counts(hunk: &crate::Hunk) -> Result<()> {
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
    if changed_lines > MAX_CHANGED_LINES {
        return Err(anyhow!(
            "hunk changes {changed_lines} lines; maximum is {MAX_CHANGED_LINES}"
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
    let mut output = String::new();

    for hunk in &diff.hunks {
        output.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len
        ));
        for line in &hunk.lines {
            let (prefix, text) = match line {
                DiffLine::Context(text) => (' ', text),
                DiffLine::Remove(text) => ('-', text),
                DiffLine::Add(text) => ('+', text),
            };
            output.push(prefix);
            output.push_str(text);
            output.push('\n');
        }
    }

    output
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
        };

        PatchValidator::validate_card_against_context(&card, &context).unwrap();
    }

    #[test]
    fn rejects_hunk_that_does_not_match_buffer_context() {
        let card = Card::Patch(pair_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
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
        };

        let error = PatchNormalizer::normalize_card(&mut card, &context).unwrap_err();

        assert!(error.to_string().contains("ambiguous"));
    }
}
