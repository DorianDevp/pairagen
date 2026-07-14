use anyhow::{Result, anyhow};

use crate::unified_diff::{DiffLine, UnifiedDiff};

pub struct PatchApply;

impl PatchApply {
    pub fn apply_to_text(text: &str, diff: &UnifiedDiff) -> Result<String> {
        let source = text.lines().map(str::to_string).collect::<Vec<_>>();
        let mut output = Vec::new();
        let mut index = 0;

        for hunk in &diff.hunks {
            let start = resolve_start(&source, hunk)?;

            while index < start {
                output.push(source.get(index).cloned().unwrap_or_default());
                index += 1;
            }

            for line in &hunk.lines {
                match line {
                    DiffLine::Context(expected) => {
                        // Keep the real source line, not the model's possibly
                        // whitespace-drifted copy of it.
                        let actual = require_line(&source, index, expected)?;
                        output.push(actual);
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

fn resolve_start(source: &[String], hunk: &crate::Hunk) -> Result<usize> {
    let expected = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            DiffLine::Context(text) | DiffLine::Remove(text) => Some(text.as_str()),
            DiffLine::Add(_) => None,
        })
        .collect::<Vec<_>>();
    let declared = hunk.old_start.saturating_sub(1);

    if matches_at(source, declared, &expected) {
        return Ok(declared);
    }

    let matches = (0..=source.len().saturating_sub(expected.len()))
        .filter(|start| matches_at(source, *start, &expected))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [start] => Ok(*start),
        [] => Err(anyhow!("patch context was not found in the current source")),
        _ => Err(anyhow!("patch context is ambiguous in the current source")),
    }
}

fn matches_at(source: &[String], start: usize, expected: &[&str]) -> bool {
    expected.iter().enumerate().all(|(index, line)| {
        source
            .get(start + index)
            .is_some_and(|actual| crate::line_matches(actual, line))
    })
}

/// Verifies the source line matches (ignoring whitespace) and returns the
/// actual source text so the applied output preserves the file verbatim.
fn require_line(source: &[String], index: usize, expected: &str) -> Result<String> {
    let Some(actual) = source.get(index) else {
        return Err(anyhow!("patch exceeds source"));
    };

    if !crate::line_matches(actual, expected) {
        return Err(anyhow!("patch context mismatch"));
    }

    Ok(actual.clone())
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

    #[test]
    fn relocates_a_hunk_by_its_exact_context() {
        let diff = UnifiedDiff::parse("@@ -1,3 +1,3 @@\n before\n-old\n+new\n after\n").unwrap();
        let output = PatchApply::apply_to_text("prefix\nbefore\nold\nafter\n", &diff).unwrap();

        assert_eq!(output, "prefix\nbefore\nnew\nafter\n");
    }

    #[test]
    fn tolerates_whitespace_drift_and_preserves_source_indentation() {
        // The model reproduced the context with the wrong leading indentation
        // and a trailing space, but the removed/added intent is unambiguous.
        let diff =
            UnifiedDiff::parse("@@ -1,3 +1,3 @@\n  guard  \n-    old\n+    new\n  tail\n").unwrap();
        let output = PatchApply::apply_to_text("\t\tguard\n\t\told\n\t\ttail\n", &diff).unwrap();

        // Context lines keep the file's real tabs; only the added line is the
        // model's text.
        assert_eq!(output, "\t\tguard\n    new\n\t\ttail\n");
    }
}
