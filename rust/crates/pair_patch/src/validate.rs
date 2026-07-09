use anyhow::{Result, anyhow};
use pair_protocol::{Card, FilePatch};

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

        for hunk in diff.hunks {
            validate_hunk_counts(&hunk.lines)?;
        }

        Ok(())
    }
}

fn validate_hunk_counts(lines: &[DiffLine]) -> Result<()> {
    let old_count = lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Context(_) | DiffLine::Remove(_)))
        .count();
    let new_count = lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Context(_) | DiffLine::Add(_)))
        .count();

    if old_count == 0 && new_count == 0 {
        return Err(anyhow!("empty hunk"));
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
}
