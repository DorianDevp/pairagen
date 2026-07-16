use anyhow::Result;
use loopbiotic_protocol::ViolationClass;

use crate::violation::violation;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnifiedDiff {
    pub hunks: Vec<Hunk>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffLine {
    Context(String),
    Remove(String),
    Add(String),
}

/// True when a source line and a patch context/remove line are equal ignoring
/// leading and trailing whitespace. Models routinely drift on indentation or
/// trailing spaces when reproducing context, which used to fail the patch
/// contract and force an expensive full re-draft. We locate the hunk with this
/// tolerant comparison and then canonicalize the diff back to the exact source
/// text, so the applied result is byte-for-byte correct — only the *matching*
/// is fuzzy, never the output. Interior whitespace differences are still a real
/// content mismatch and do not match.
pub fn line_matches(source_line: &str, patch_line: &str) -> bool {
    source_line.trim() == patch_line.trim()
}

impl UnifiedDiff {
    pub fn parse(diff: &str) -> Result<Self> {
        let mut hunks = Vec::new();
        let mut current = None;

        for line in diff.lines() {
            if line.starts_with("@@") {
                if let Some(hunk) = current.take() {
                    hunks.push(hunk);
                }

                current = Some(parse_hunk(line)?);
                continue;
            }

            let Some(hunk) = current.as_mut() else {
                if line.trim().is_empty() {
                    continue;
                }
                return Err(violation(
                    ViolationClass::MalformedDiff,
                    format!("unexpected content before first diff hunk: {line}"),
                ));
            };

            if let Some(text) = line.strip_prefix(' ') {
                hunk.lines.push(DiffLine::Context(text.into()));
            } else if let Some(text) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine::Remove(text.into()));
            } else if let Some(text) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine::Add(text.into()));
            } else if line == "\\ No newline at end of file" {
                continue;
            } else {
                return Err(violation(
                    ViolationClass::MalformedDiff,
                    format!("invalid diff line {line}"),
                ));
            }
        }

        if let Some(hunk) = current {
            hunks.push(hunk);
        }

        if hunks.is_empty() {
            return Err(violation(
                ViolationClass::MalformedDiff,
                "diff has no hunks",
            ));
        }

        Ok(Self { hunks })
    }

    pub fn render(&self) -> String {
        let mut output = String::new();

        for hunk in &self.hunks {
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
}

fn parse_hunk(line: &str) -> Result<Hunk> {
    let parts = line.split_whitespace().collect::<Vec<_>>();

    if parts.len() < 3 {
        return Err(violation(
            ViolationClass::MalformedDiff,
            "invalid hunk header",
        ));
    }

    let (old_start, old_len) = parse_range(parts[1], '-')?;
    let (new_start, new_len) = parse_range(parts[2], '+')?;

    Ok(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        lines: vec![],
    })
}

fn parse_range(value: &str, prefix: char) -> Result<(usize, usize)> {
    let value = value
        .strip_prefix(prefix)
        .ok_or_else(|| violation(ViolationClass::MalformedDiff, "invalid hunk range"))?;
    let mut parts = value.split(',');
    let start = parts
        .next()
        .ok_or_else(|| violation(ViolationClass::MalformedDiff, "missing range start"))?
        .parse()
        .map_err(|_| violation(ViolationClass::MalformedDiff, "invalid hunk range start"))?;
    let len = parts
        .next()
        .unwrap_or("1")
        .parse()
        .map_err(|_| violation(ViolationClass::MalformedDiff, "invalid hunk range length"))?;

    Ok((start, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunk() {
        let diff = "@@ -1,1 +1,1 @@\n-old\n+new\n";
        let parsed = UnifiedDiff::parse(diff).unwrap();

        assert_eq!(parsed.hunks.len(), 1);
        assert_eq!(parsed.hunks[0].old_start, 1);
    }

    #[test]
    fn rejects_unhandled_content_before_the_first_hunk() {
        let error =
            UnifiedDiff::parse("--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n")
                .unwrap_err();

        assert_eq!(
            crate::violation_class(&error),
            Some(ViolationClass::MalformedDiff)
        );
        assert!(error.to_string().contains("before first diff hunk"));
    }
}
