use anyhow::{Result, anyhow};
use pair_protocol::{
    Card, ContextBundle, FilePatch, MAX_CHANGED_LINES, MAX_HUNKS_PER_PATCH, MAX_PATCH_FILES,
};

use crate::unified_diff::{DiffLine, UnifiedDiff};

pub struct PatchValidator;

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
}
