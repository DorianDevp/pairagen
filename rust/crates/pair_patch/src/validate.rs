use anyhow::{Result, anyhow};
use pair_protocol::{Card, FilePatch, MAX_CHANGED_LINES, MAX_HUNKS_PER_PATCH, MAX_PATCH_FILES};

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

    if old_count == 0 && new_count == 0 {
        return Err(anyhow!("empty hunk"));
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
}
