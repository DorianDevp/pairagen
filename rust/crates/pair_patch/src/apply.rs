use anyhow::{Result, anyhow};

use crate::unified_diff::{DiffLine, UnifiedDiff};

pub struct PatchApply;

impl PatchApply {
    pub fn apply_to_text(text: &str, diff: &UnifiedDiff) -> Result<String> {
        let source = text.lines().map(str::to_string).collect::<Vec<_>>();
        let mut output = Vec::new();
        let mut index = 0;

        for hunk in &diff.hunks {
            let start = hunk.old_start.saturating_sub(1);

            while index < start {
                output.push(source.get(index).cloned().unwrap_or_default());
                index += 1;
            }

            for line in &hunk.lines {
                match line {
                    DiffLine::Context(expected) => {
                        require_line(&source, index, expected)?;
                        output.push(expected.clone());
                        index += 1;
                    }
                    DiffLine::Remove(expected) => {
                        require_line(&source, index, expected)?;
                        index += 1;
                    }
                    DiffLine::Add(value) => output.push(value.clone()),
                }
            }
        }

        while index < source.len() {
            output.push(source[index].clone());
            index += 1;
        }

        Ok(format!("{}\n", output.join("\n")))
    }
}

fn require_line(source: &[String], index: usize, expected: &str) -> Result<()> {
    let Some(actual) = source.get(index) else {
        return Err(anyhow!("patch exceeds source"));
    };

    if actual != expected {
        return Err(anyhow!("patch context mismatch"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::unified_diff::UnifiedDiff;

    use super::*;

    #[test]
    fn applies_simple_patch() {
        let diff = UnifiedDiff::parse("@@ -1,2 +1,2 @@\n one\n-old\n+new\n").unwrap();
        let output = PatchApply::apply_to_text("one\nold\n", &diff).unwrap();

        assert_eq!(output, "one\nnew\n");
    }
}
