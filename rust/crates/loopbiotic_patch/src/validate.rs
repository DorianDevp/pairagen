use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use loopbiotic_protocol::{
    Card, ContextBundle, FileOp, FilePatch, MAX_CHANGED_LINES, MAX_FILE_OPS, MAX_HUNKS_PER_PATCH,
    MAX_PATCH_FILES, ViolationClass,
};

use crate::unified_diff::{DiffLine, UnifiedDiff};
use crate::violation::violation;

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
        let expected_file = workspace_relative_file(context);

        for patch in &mut card.patches {
            normalize_patch_file(&mut patch.file, Some(&expected_file));
            patch.diff = strip_diff_envelope(&patch.diff, &patch.file)?;
            // Models drafting against an LF buffer sometimes emit CRLF line
            // endings; drop the mechanical `\r` so added lines match the
            // buffer's endings. A buffer that itself carries `\r` makes the
            // intent ambiguous, so it is left for the retry path.
            if patch.diff.contains('\r') && !context.buffer_text.contains('\r') {
                patch.diff = patch.diff.replace("\r\n", "\n");
            }
            let mut diff = UnifiedDiff::parse(&patch.diff)?;
            for hunk in &mut diff.hunks {
                // Models routinely miscount the `@@ -a,b +c,d @@` ranges. The
                // counts are pure functions of the line list, so correct them
                // instead of failing the contract and forcing a re-draft.
                recompute_hunk_lengths(hunk);

                // Locate the hunk in `start`, dropping the borrow of `hunk.lines`
                // before we canonicalize them below. `None` means an empty-file
                // create that needs no source anchoring.
                let start = {
                    let expected = hunk
                        .lines
                        .iter()
                        .filter_map(|line| match line {
                            DiffLine::Context(text) | DiffLine::Remove(text) => Some(text.as_str()),
                            DiffLine::Add(_) => None,
                        })
                        .collect::<Vec<_>>();
                    if expected.is_empty() {
                        if hunk.old_len == 0 && source.is_empty() {
                            hunk.old_start = context.buffer_start_line;
                            hunk.new_start = context.buffer_start_line;
                            None
                        } else {
                            return Err(violation(
                                ViolationClass::ContextMismatch,
                                "patch hunk without source context can only create an empty file",
                            ));
                        }
                    } else {
                        let declared = hunk.old_start.checked_sub(context.buffer_start_line);
                        let located = if declared
                            .is_some_and(|start| matches_at(&source, start, &expected))
                        {
                            declared.unwrap()
                        } else {
                            let matches = (0..=source.len().saturating_sub(expected.len()))
                                .filter(|start| matches_at(&source, *start, &expected))
                                .collect::<Vec<_>>();
                            match matches.as_slice() {
                                [start] => *start,
                                [] => {
                                    return Err(violation(
                                        ViolationClass::ContextMismatch,
                                        "patch context was not found in the supplied buffer",
                                    ));
                                }
                                _ => {
                                    return Err(violation(
                                        ViolationClass::ContextMismatch,
                                        "patch context is ambiguous in the supplied buffer",
                                    ));
                                }
                            }
                        };
                        Some(located)
                    }
                };

                let Some(start) = start else {
                    continue;
                };

                // Rewrite context/remove lines to the exact source text so the
                // diff applies byte-exact downstream even if the model drifted
                // on whitespace. Added lines are never touched.
                let mut src_pos = start;
                for line in &mut hunk.lines {
                    match line {
                        DiffLine::Context(text) | DiffLine::Remove(text) => {
                            if let Some(actual) = source.get(src_pos) {
                                *text = (*actual).to_string();
                            }
                            src_pos += 1;
                        }
                        DiffLine::Add(_) => {}
                    }
                }

                let corrected_old_start = context.buffer_start_line + start;
                let delta = hunk.new_start as isize - hunk.old_start as isize;
                let corrected_new_start = corrected_old_start
                    .checked_add_signed(delta)
                    .ok_or_else(|| {
                        violation(
                            ViolationClass::HunkHeaderMismatch,
                            "corrected patch coordinates are outside the file",
                        )
                    })?;
                if corrected_new_start == 0 {
                    return Err(violation(
                        ViolationClass::HunkHeaderMismatch,
                        "corrected patch coordinates must start at 1",
                    ));
                }

                hunk.old_start = corrected_old_start;
                hunk.new_start = corrected_new_start;
            }
            patch.diff = render_diff(&diff);
        }

        Ok(())
    }

    /// Correct each hunk's `@@ -a,b +c,d @@` line counts from its actual lines,
    /// without needing source context. Runs before validation on a raw card so
    /// a miscounted header (a very common model error) is fixed rather than
    /// rejected — the counts are fully derivable from the body.
    pub fn normalize_hunk_headers(card: &mut Card) -> Result<()> {
        let Card::Patch(card) = card else {
            return Ok(());
        };

        for patch in &mut card.patches {
            // No context is available here, so only the always-safe repairs
            // run: `./` on the target path and the diff envelope.
            normalize_patch_file(&mut patch.file, None);
            patch.diff = strip_diff_envelope(&patch.diff, &patch.file)?;
            let mut diff = UnifiedDiff::parse(&patch.diff)?;
            for hunk in &mut diff.hunks {
                recompute_hunk_lengths(hunk);
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

        if !card.file_ops.is_empty() {
            if !card.patches.is_empty() {
                return Err(violation(
                    ViolationClass::MissingField,
                    "patch card cannot mix file patches and file_ops; move first, then edit content in the next goal step",
                ));
            }
            return Self::validate_file_ops(&card.file_ops);
        }

        if card.patches.is_empty() {
            return Err(violation(
                ViolationClass::MissingField,
                "patch card has no patches",
            ));
        }
        if card.patches.len() > max_patch_files {
            return Err(violation(
                ViolationClass::MultiHunk,
                format!(
                    "patch card changes {} files; maximum is {max_patch_files}",
                    card.patches.len(),
                ),
            ));
        }

        for patch in &card.patches {
            Self::validate_file_patch_with_limits(patch, max_hunks_per_patch, max_changed_lines)?;
        }

        Ok(())
    }

    /// Filesystem operations carried by a patch card: workspace-relative,
    /// traversal-free, bounded, and mutually independent. The editor
    /// revalidates against the live filesystem before Accept applies them.
    pub fn validate_file_ops(file_ops: &[FileOp]) -> Result<()> {
        if file_ops.len() > MAX_FILE_OPS {
            return Err(violation(
                ViolationClass::MultiHunk,
                format!(
                    "patch card proposes {} file operations; maximum is {MAX_FILE_OPS}",
                    file_ops.len(),
                ),
            ));
        }

        let mut seen = std::collections::HashSet::new();
        for op in file_ops {
            if op.id.trim().is_empty() {
                return Err(violation(
                    ViolationClass::MissingField,
                    "file operation id is empty",
                ));
            }
            for (label, path) in [("from", &op.from), ("to", &op.to)] {
                if path.as_os_str().is_empty() {
                    return Err(violation(
                        ViolationClass::MissingField,
                        format!("file operation {label} path is empty"),
                    ));
                }
                if path.is_absolute() {
                    return Err(violation(
                        ViolationClass::WrongFile,
                        format!(
                            "file operation {label} path must be workspace-relative: {}",
                            path.display()
                        ),
                    ));
                }
                if path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
                {
                    return Err(violation(
                        ViolationClass::WrongFile,
                        format!(
                            "file operation {label} path escapes the workspace: {}",
                            path.display()
                        ),
                    ));
                }
            }
            if op.from == op.to {
                return Err(violation(
                    ViolationClass::Other,
                    format!("file operation moves {} onto itself", op.from.display()),
                ));
            }
            if op.to.starts_with(&op.from) {
                return Err(violation(
                    ViolationClass::Other,
                    format!("file operation moves {} into itself", op.from.display()),
                ));
            }
            if !seen.insert(op.from.clone()) || !seen.insert(op.to.clone()) {
                return Err(violation(
                    ViolationClass::DuplicateStep,
                    "file operations reuse the same path",
                ));
            }
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
            return Err(violation(ViolationClass::MissingField, "patch id is empty"));
        }

        if patch.file.as_os_str().is_empty() {
            return Err(violation(
                ViolationClass::MissingField,
                "patch file is empty",
            ));
        }

        if patch.file.is_absolute() {
            return Err(violation(
                ViolationClass::WrongFile,
                "patch file must be relative",
            ));
        }

        let diff = UnifiedDiff::parse(&patch.diff)?;
        if diff.hunks.len() > max_hunks_per_patch {
            return Err(violation(
                ViolationClass::MultiHunk,
                format!(
                    "patch has {} hunks; maximum is {max_hunks_per_patch}",
                    diff.hunks.len(),
                ),
            ));
        }

        for hunk in diff.hunks {
            validate_single_change_run(&hunk)?;
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
                        violation(
                            ViolationClass::ContextMismatch,
                            "patch hunk starts before the supplied buffer excerpt",
                        )
                    })?;
                let expected = hunk
                    .lines
                    .iter()
                    .filter_map(|line| match line {
                        DiffLine::Context(text) | DiffLine::Remove(text) => Some(text.as_str()),
                        DiffLine::Add(_) => None,
                    })
                    .collect::<Vec<_>>();

                if expected.is_empty() {
                    if hunk.old_len == 0 && source.is_empty() && start == 0 {
                        continue;
                    }
                    return Err(violation(
                        ViolationClass::ContextMismatch,
                        "patch hunk without source context can only create an empty file",
                    ));
                }

                for (offset, expected) in expected.into_iter().enumerate() {
                    let line = context.buffer_start_line + start + offset;
                    let actual = source.get(start + offset).copied().ok_or_else(|| {
                        violation(
                            ViolationClass::ContextMismatch,
                            format!(
                                "patch source context at line {line} is outside the supplied buffer"
                            ),
                        )
                    })?;
                    if actual != expected {
                        return Err(violation(
                            ViolationClass::ContextMismatch,
                            format!(
                                "patch source context mismatch at line {line}: expected {expected:?}, got {actual:?}"
                            ),
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

/// A unified-diff `@@` header may legally merge multiple edits separated by
/// context lines. For Loopbiotic those are still multiple review steps: each
/// accepted card must contain one contiguous change block so dependency-first
/// work can be reviewed and compiled between steps.
fn validate_single_change_run(hunk: &crate::Hunk) -> Result<()> {
    let mut runs = 0;
    let mut changing = false;

    for line in &hunk.lines {
        let changed = matches!(line, DiffLine::Remove(_) | DiffLine::Add(_));
        if changed && !changing {
            runs += 1;
        }
        changing = changed;
    }

    if runs > 1 {
        return Err(violation(
            ViolationClass::MultiHunk,
            format!(
                "patch contains {runs} separate change blocks inside one @@ hunk; maximum is 1. Split them into sequential compiler-safe patches"
            ),
        ));
    }

    Ok(())
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
        if hunk.old_len != 0 {
            return Err(violation(
                ViolationClass::HunkHeaderMismatch,
                "hunk has no source context",
            ));
        }
        if !hunk
            .lines
            .iter()
            .any(|line| matches!(line, DiffLine::Add(_)))
        {
            return Err(violation(
                ViolationClass::MalformedDiff,
                "new-file hunk has no added lines",
            ));
        }
    }
    if old_count != hunk.old_len || new_count != hunk.new_len {
        return Err(violation(
            ViolationClass::HunkHeaderMismatch,
            "hunk header counts do not match its lines",
        ));
    }

    let changed_lines = hunk
        .lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Remove(_) | DiffLine::Add(_)))
        .count();
    if changed_lines > max_changed_lines {
        return Err(violation(
            ViolationClass::MultiHunk,
            format!("hunk changes {changed_lines} lines; maximum is {max_changed_lines}"),
        ));
    }

    Ok(())
}

fn matches_at(source: &[&str], start: usize, expected: &[&str]) -> bool {
    expected.iter().enumerate().all(|(offset, line)| {
        source
            .get(start + offset)
            .is_some_and(|actual| crate::line_matches(actual, line))
    })
}

fn render_diff(diff: &UnifiedDiff) -> String {
    diff.render()
}

/// The workspace-relative form of the context's file — the form patch cards
/// are required to target.
fn workspace_relative_file(context: &ContextBundle) -> PathBuf {
    if context.file.is_absolute() {
        context
            .file
            .strip_prefix(&context.cwd)
            .unwrap_or(&context.file)
            .to_path_buf()
    } else {
        context.file.clone()
    }
}

/// Repairs mechanical prefixes on a patch target path. A leading `./` is an
/// identity and always dropped; git's `a/`/`b/` prefixes are only dropped
/// when the stripped path is exactly the expected workspace-relative target,
/// since a project may genuinely contain an `a/` or `b/` directory.
fn normalize_patch_file(file: &mut PathBuf, expected: Option<&Path>) {
    if let Ok(stripped) = file.strip_prefix("./")
        && !stripped.as_os_str().is_empty()
    {
        *file = stripped.to_path_buf();
    }
    if let Some(expected) = expected
        && file != expected
        && let Some(stripped) = strip_diff_path_prefix(file)
        && stripped == expected
    {
        *file = stripped;
    }
}

/// Returns the path without git's mechanical `a/` or `b/` diff prefix, or
/// `None` when it carries no such prefix.
pub fn strip_diff_path_prefix(file: &Path) -> Option<PathBuf> {
    ["a", "b"].iter().find_map(|prefix| {
        file.strip_prefix(prefix)
            .ok()
            .filter(|stripped| !stripped.as_os_str().is_empty())
            .map(Path::to_path_buf)
    })
}

/// Strips a mechanical model-added envelope from a diff: markdown code fences
/// wrapping it, and a leading git-style header block whose paths already name
/// `file` (modulo `a/`, `b/`, and `./` prefixes) or `/dev/null`. Anything
/// else before the first hunk — prose, headers naming another file,
/// rename/copy headers — is rejected so it cannot be silently discarded by a
/// parser or applied to the wrong target.
fn strip_diff_envelope(diff: &str, file: &Path) -> Result<String> {
    let lines = diff.lines().collect::<Vec<_>>();
    let mut start = 0;
    let mut end = lines.len();

    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }

    let has_opening_fence = start < end && is_code_fence(lines[start]);
    let has_closing_fence = end > start && lines[end - 1].trim() == "```";
    match (has_opening_fence, has_closing_fence) {
        (true, true) => {
            start += 1;
            end -= 1;
        }
        (false, false) => {}
        _ => {
            return Err(violation(
                ViolationClass::MalformedDiff,
                "diff has an unmatched markdown code fence",
            ));
        }
    }

    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }

    // A leading git header block, verified line by line.
    while start < end && !lines[start].starts_with("@@") {
        let Some(paths) = header_line_paths(lines[start]) else {
            return Err(violation(
                ViolationClass::MalformedDiff,
                format!(
                    "unexpected content before first diff hunk: {}",
                    lines[start]
                ),
            ));
        };
        if !paths.iter().all(|path| header_path_matches(path, file)) {
            return Err(violation(
                ViolationClass::WrongFile,
                format!(
                    "diff header targets a different file than {}",
                    file.display()
                ),
            ));
        }
        start += 1;
    }

    if !lines.get(start).is_some_and(|line| line.starts_with("@@")) {
        return Err(violation(
            ViolationClass::MalformedDiff,
            "diff has no hunks",
        ));
    }

    let mut body = lines[start..end].join("\n");
    body.push('\n');
    Ok(body)
}

fn is_code_fence(line: &str) -> bool {
    line.trim()
        .strip_prefix("```")
        .is_some_and(|language| language.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Recognizes one line of a git-style diff header, returning the paths it
/// names (empty when it names none). `None` means the line is not a purely
/// mechanical header — rename/copy headers carry intent and are not stripped.
fn header_line_paths(line: &str) -> Option<Vec<&str>> {
    if let Some(rest) = line.strip_prefix("diff --git ") {
        return Some(rest.split_whitespace().collect());
    }
    if let Some(rest) = line
        .strip_prefix("--- ")
        .or_else(|| line.strip_prefix("+++ "))
    {
        // The classic format may append a tab-separated timestamp.
        return Some(vec![rest.split('\t').next().unwrap_or(rest).trim()]);
    }

    const PATHLESS: &[&str] = &[
        "index ",
        "new file mode ",
        "deleted file mode ",
        "old mode ",
        "new mode ",
    ];
    PATHLESS
        .iter()
        .any(|prefix| line.starts_with(prefix))
        .then(Vec::new)
}

fn header_path_matches(path: &str, file: &Path) -> bool {
    if path == "/dev/null" {
        return true;
    }
    let path = Path::new(path);
    let path = path.strip_prefix("./").unwrap_or(path);

    path == file || strip_diff_path_prefix(path).is_some_and(|stripped| stripped == file)
}

fn recompute_hunk_lengths(hunk: &mut crate::Hunk) {
    hunk.old_len = hunk
        .lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Context(_) | DiffLine::Remove(_)))
        .count();
    hunk.new_len = hunk
        .lines
        .iter()
        .filter(|line| matches!(line, DiffLine::Context(_) | DiffLine::Add(_)))
        .count();
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

    use loopbiotic_protocol::FilePatch;

    use super::*;

    fn file_op(id: &str, from: &str, to: &str) -> FileOp {
        FileOp {
            id: id.into(),
            kind: loopbiotic_protocol::FileOpKind::Move,
            from: PathBuf::from(from),
            to: PathBuf::from(to),
        }
    }

    fn ops_card(file_ops: Vec<FileOp>, patches: Vec<FilePatch>) -> Card {
        Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_ops".into(),
            title: "Move files".into(),
            explanation: "Group the module.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches,
            file_ops,
            actions: vec![loopbiotic_protocol::Action::Apply],
        })
    }

    #[test]
    fn accepts_a_file_ops_only_patch_card() {
        let card = ops_card(vec![file_op("fo_1", "src/a.ts", "src/lib/a.ts")], vec![]);

        PatchValidator::validate_card(&card).unwrap();
    }

    #[test]
    fn rejects_a_card_mixing_patches_and_file_ops() {
        let card = ops_card(
            vec![file_op("fo_1", "src/a.ts", "src/lib/a.ts")],
            vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/a.ts"),
                diff: "@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
                explanation: "E".into(),
            }],
        );

        let error = PatchValidator::validate_card(&card).unwrap_err();
        assert!(error.to_string().contains("cannot mix"));
    }

    #[test]
    fn rejects_file_ops_that_escape_or_overlap() {
        for (from, to, fragment) in [
            ("/abs/a.ts", "src/a.ts", "workspace-relative"),
            ("src/a.ts", "../outside.ts", "escapes the workspace"),
            ("src/a.ts", "src/a.ts", "onto itself"),
            ("src/dir", "src/dir/inner", "into itself"),
        ] {
            let card = ops_card(vec![file_op("fo_1", from, to)], vec![]);
            let error = PatchValidator::validate_card(&card).unwrap_err();
            assert!(
                error.to_string().contains(fragment),
                "{from} -> {to}: {error}"
            );
        }

        let duplicated = ops_card(
            vec![
                file_op("fo_1", "src/a.ts", "src/lib/a.ts"),
                file_op("fo_2", "src/b.ts", "src/lib/a.ts"),
            ],
            vec![],
        );
        let error = PatchValidator::validate_card(&duplicated).unwrap_err();
        assert!(error.to_string().contains("reuse the same path"));
    }

    #[test]
    fn rejects_too_many_file_ops() {
        let ops = (0..MAX_FILE_OPS + 1)
            .map(|index| {
                file_op(
                    &format!("fo_{index}"),
                    &format!("src/file{index}.ts"),
                    &format!("src/lib/file{index}.ts"),
                )
            })
            .collect();

        let error = PatchValidator::validate_card(&ops_card(ops, vec![])).unwrap_err();
        assert!(error.to_string().contains("maximum is"));
    }

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
    fn rejects_two_change_blocks_hidden_inside_one_hunk_header() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,4 +1,5 @@\n+interface Work {}\n context one\n context two\n-old_type\n+Work\n context three\n"
                .into(),
            explanation: String::new(),
        };

        let error = PatchValidator::validate_file_patch(&patch).unwrap_err();

        assert_eq!(
            crate::violation_class(&error),
            Some(ViolationClass::MultiHunk)
        );
        assert!(error.to_string().contains("2 separate change blocks"));
        assert!(error.to_string().contains("compiler-safe patches"));
    }

    #[test]
    fn accepts_one_replacement_change_run() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,3 +1,3 @@\n before\n-old_type\n+NewType\n after\n".into(),
            explanation: String::new(),
        };

        PatchValidator::validate_file_patch(&patch).unwrap();
    }

    #[test]
    fn work_turns_do_not_bypass_the_review_limit() {
        let mut diff = "@@ -1,20 +1,20 @@\n".to_string();
        for index in 0..20 {
            diff.push_str(&format!("-line {index}\n"));
        }
        for index in 0..20 {
            diff.push_str(&format!("+    line {index}\n"));
        }
        let card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_format".into(),
            title: "Format".into(),
            explanation: "Indent the block.".into(),
            warnings: vec![],
            goal_complete: true,
            plan: None,
            patches: vec![FilePatch {
                id: "p_format".into(),
                file: PathBuf::from("templates/view.html"),
                diff,
                explanation: "Indent the block.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });

        assert!(PatchValidator::validate_card(&card).is_err());
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
    fn accepts_new_file_hunk_syntax() {
        let patch = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,0 +1,1 @@\n+new\n".into(),
            explanation: "Insert a line.".into(),
        };

        PatchValidator::validate_file_patch(&patch).unwrap();
    }

    #[test]
    fn validates_new_file_hunk_against_an_empty_buffer() {
        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_new".into(),
            title: "Create exception".into(),
            explanation: "Add the missing exception type.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_new".into(),
                file: PathBuf::from("src/Exception/NewException.php"),
                diff: "@@ -1,0 +1,3 @@\n+<?php\n+\n+final class NewException {}\n".into(),
                explanation: "Create the exception.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/Exception/NewException.php"),
            cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: String::new(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
            call_hierarchy: None,
        };

        PatchNormalizer::normalize_card(&mut card, &context).unwrap();
        PatchValidator::validate_card_against_context(&card, &context).unwrap();
    }

    #[test]
    fn rejects_context_free_insert_into_an_existing_buffer() {
        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_insert".into(),
            title: "Insert".into(),
            explanation: "Insert without an anchor.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_insert".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -1,0 +1,1 @@\n+new\n".into(),
                explanation: "Insert a line.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "existing".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
            call_hierarchy: None,
        };

        let error = PatchNormalizer::normalize_card(&mut card, &context).unwrap_err();
        assert!(error.to_string().contains("empty file"));
    }

    #[test]
    fn validates_hunk_against_buffer_file_coordinates() {
        let card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -51,1 +51,1 @@\n-old\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: loopbiotic_protocol::Cursor {
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
            call_hierarchy: None,
        };

        PatchValidator::validate_card_against_context(&card, &context).unwrap();
    }

    #[test]
    fn rejects_hunk_that_does_not_match_buffer_context() {
        let card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -2,1 +2,1 @@\n-stale\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: loopbiotic_protocol::Cursor { line: 2, column: 1 },
            selection: None,
            buffer_text: "before\ncurrent\nafter".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
            call_hierarchy: None,
        };

        let error = PatchValidator::validate_card_against_context(&card, &context).unwrap_err();

        assert!(error.to_string().contains("mismatch at line 2"));
    }

    #[test]
    fn normalizes_uniquely_relocated_hunk_coordinates() {
        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -52,2 +52,2 @@\n marker\n-old\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: loopbiotic_protocol::Cursor {
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
            call_hierarchy: None,
        };

        PatchNormalizer::normalize_card(&mut card, &context).unwrap();
        PatchValidator::validate_card_against_context(&card, &context).unwrap();

        let Card::Patch(card) = card else {
            unreachable!();
        };
        assert!(card.patches[0].diff.starts_with("@@ -50,2 +50,2 @@"));
    }

    #[test]
    fn recomputes_miscounted_hunk_headers_instead_of_rejecting() {
        // Header claims 9,9 but the body is a single-line change. Raw validation
        // rejects it; after normalizing headers it passes and the header is fixed.
        let raw = FilePatch {
            id: "p_1".into(),
            file: PathBuf::from("src/work.ts"),
            diff: "@@ -1,9 +1,9 @@\n context\n-old\n+new\n".into(),
            explanation: "Fix.".into(),
        };
        assert!(
            PatchValidator::validate_file_patch(&raw)
                .unwrap_err()
                .to_string()
                .contains("header counts")
        );

        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Fix".into(),
            explanation: "Fix.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![raw],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });

        PatchNormalizer::normalize_hunk_headers(&mut card).unwrap();
        PatchValidator::validate_card(&card).unwrap();

        let Card::Patch(card) = card else {
            unreachable!()
        };
        assert!(card.patches[0].diff.starts_with("@@ -1,2 +1,2 @@"));
    }

    #[test]
    fn normalizes_a_whitespace_drifted_hunk_to_exact_source() {
        // Context/remove lines drift on indentation and trailing space; the
        // normalizer must still locate the hunk and canonicalize it so the
        // downstream exact-match validator passes.
        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Fix".into(),
            explanation: "Fix the guard.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -1,2 +1,2 @@\n guard  \n-    old\n+    new\n".into(),
                explanation: "Fix.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "\tguard\n\told\n".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
            call_hierarchy: None,
        };

        PatchNormalizer::normalize_card(&mut card, &context).unwrap();
        // The exact-match validator would fail on the original drifted diff;
        // it passes because normalization rewrote context/remove to the source.
        PatchValidator::validate_card_against_context(&card, &context).unwrap();

        let Card::Patch(card) = card else {
            unreachable!()
        };
        assert!(card.patches[0].diff.contains(" \tguard"));
        assert!(card.patches[0].diff.contains("-\told"));
        assert!(card.patches[0].diff.contains("+    new"));
    }

    #[test]
    fn refuses_to_relocate_ambiguous_hunk_context() {
        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename".into(),
            explanation: "Rename the value.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.ts"),
                diff: "@@ -9,1 +9,1 @@\n-old\n+new\n".into(),
                explanation: "Rename one line.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });
        let context = ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from("src/work.ts"),
            cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: "old\nbetween\nold".into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
            call_hierarchy: None,
        };

        let error = PatchNormalizer::normalize_card(&mut card, &context).unwrap_err();

        assert!(error.to_string().contains("ambiguous"));
    }

    fn patch_card(patch: FilePatch) -> Card {
        Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Fix".into(),
            explanation: "Fix.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![patch],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        })
    }

    fn file_patch(file: &str, diff: &str) -> FilePatch {
        FilePatch {
            id: "p_1".into(),
            file: PathBuf::from(file),
            diff: diff.into(),
            explanation: "Fix.".into(),
        }
    }

    fn context(file: &str, buffer_text: &str) -> ContextBundle {
        ContextBundle {
            cwd: PathBuf::from("/tmp/project"),
            file: PathBuf::from(file),
            cursor: loopbiotic_protocol::Cursor { line: 1, column: 1 },
            selection: None,
            buffer_text: buffer_text.into(),
            buffer_start_line: 1,
            diagnostics: vec![],
            hints: vec![],
            artifacts: vec![],
            report: None,
            call_hierarchy: None,
        }
    }

    fn normalized_diff(card: Card) -> String {
        let Card::Patch(card) = card else {
            unreachable!()
        };
        card.patches[0].diff.clone()
    }

    const HUNK: &str = "@@ -1,1 +1,1 @@\n-old\n+new\n";

    #[test]
    fn envelope_passes_a_plain_diff_through_untouched() {
        let mut card = patch_card(file_patch("src/work.ts", HUNK));

        PatchNormalizer::normalize_hunk_headers(&mut card).unwrap();

        assert_eq!(normalized_diff(card), HUNK);
    }

    #[test]
    fn envelope_strips_markdown_fences_around_the_diff() {
        for wrapped in [
            format!("```\n{HUNK}```\n"),
            format!("```diff\n{HUNK}```"),
            format!("```diff\n{HUNK}```\n\n"),
        ] {
            let mut card = patch_card(file_patch("src/work.ts", &wrapped));

            PatchNormalizer::normalize_hunk_headers(&mut card).unwrap();

            assert_eq!(normalized_diff(card), HUNK, "input: {wrapped:?}");
        }
    }

    #[test]
    fn envelope_strips_a_git_header_block_naming_the_target_file() {
        let wrapped = format!(
            "diff --git a/src/work.ts b/src/work.ts\nindex 1111111..2222222 100644\n--- a/src/work.ts\n+++ b/src/work.ts\n{HUNK}"
        );
        let mut card = patch_card(file_patch("src/work.ts", &wrapped));

        PatchNormalizer::normalize_hunk_headers(&mut card).unwrap();

        assert_eq!(normalized_diff(card), HUNK);
    }

    #[test]
    fn envelope_strips_a_new_file_header_with_dev_null_source() {
        let wrapped = format!("--- /dev/null\n+++ b/src/work.ts\n{HUNK}");
        let mut card = patch_card(file_patch("src/work.ts", &wrapped));

        PatchNormalizer::normalize_hunk_headers(&mut card).unwrap();

        assert_eq!(normalized_diff(card), HUNK);
    }

    #[test]
    fn envelope_strips_fences_and_header_together() {
        let wrapped = format!("```diff\n--- a/src/work.ts\n+++ b/src/work.ts\n{HUNK}```\n");
        let mut card = patch_card(file_patch("src/work.ts", &wrapped));

        PatchNormalizer::normalize_hunk_headers(&mut card).unwrap();

        assert_eq!(normalized_diff(card), HUNK);
    }

    #[test]
    fn envelope_rejects_a_header_naming_another_file() {
        let wrapped = format!("--- a/src/other.ts\n+++ b/src/other.ts\n{HUNK}");
        let mut card = patch_card(file_patch("src/work.ts", &wrapped));

        let error = PatchNormalizer::normalize_hunk_headers(&mut card).unwrap_err();

        assert_eq!(
            crate::violation_class(&error),
            Some(ViolationClass::WrongFile)
        );
    }

    #[test]
    fn envelope_rejects_a_rename_header() {
        let wrapped = format!("rename from src/old.ts\nrename to src/work.ts\n{HUNK}");
        let mut card = patch_card(file_patch("src/work.ts", &wrapped));

        let error = PatchNormalizer::normalize_hunk_headers(&mut card).unwrap_err();

        assert_eq!(
            crate::violation_class(&error),
            Some(ViolationClass::MalformedDiff)
        );
    }

    #[test]
    fn envelope_rejects_prose_and_a_fenced_non_diff() {
        let prose = format!("Here is the fix:\n```diff\n{HUNK}```\n");
        let mut card = patch_card(file_patch("src/work.ts", &prose));
        let error = PatchNormalizer::normalize_hunk_headers(&mut card).unwrap_err();
        assert_eq!(
            crate::violation_class(&error),
            Some(ViolationClass::MalformedDiff)
        );

        let mut card = patch_card(file_patch("src/work.ts", "```\nnot a diff\n```\n"));
        let error = PatchNormalizer::normalize_hunk_headers(&mut card).unwrap_err();
        assert_eq!(
            crate::violation_class(&error),
            Some(ViolationClass::MalformedDiff)
        );
    }

    #[test]
    fn envelope_rejects_unmatched_markdown_fences() {
        for wrapped in [format!("```diff\n{HUNK}"), format!("{HUNK}```\n")] {
            let mut card = patch_card(file_patch("src/work.ts", &wrapped));

            let error = PatchNormalizer::normalize_hunk_headers(&mut card).unwrap_err();

            assert_eq!(
                crate::violation_class(&error),
                Some(ViolationClass::MalformedDiff),
                "input: {wrapped:?}"
            );
            assert!(error.to_string().contains("unmatched"));
        }
    }

    #[test]
    fn normalizes_a_crlf_diff_against_an_lf_buffer() {
        let mut card = patch_card(file_patch(
            "src/work.ts",
            "@@ -1,1 +1,1 @@\r\n-old\r\n+new\r\n",
        ));
        let context = context("src/work.ts", "old");

        PatchNormalizer::normalize_card(&mut card, &context).unwrap();
        PatchValidator::validate_card_against_context(&card, &context).unwrap();

        let diff = normalized_diff(card);
        assert_eq!(diff, HUNK);
        assert!(!diff.contains('\r'));
    }

    #[test]
    fn crlf_diff_with_wrong_context_still_fails() {
        let mut card = patch_card(file_patch(
            "src/work.ts",
            "@@ -1,1 +1,1 @@\r\n-stale\r\n+new\r\n",
        ));
        let context = context("src/work.ts", "current");

        let error = PatchNormalizer::normalize_card(&mut card, &context).unwrap_err();

        assert!(error.to_string().contains("not found"));
    }

    #[test]
    fn normalizes_current_dir_and_git_prefixes_on_the_patch_path() {
        for prefixed in ["./src/work.ts", "a/src/work.ts", "b/src/work.ts"] {
            let mut card = patch_card(file_patch(prefixed, HUNK));
            let context = context("src/work.ts", "old");

            PatchNormalizer::normalize_card(&mut card, &context).unwrap();

            let Card::Patch(card) = card else {
                unreachable!()
            };
            assert_eq!(
                card.patches[0].file,
                PathBuf::from("src/work.ts"),
                "input: {prefixed}"
            );
        }
    }

    #[test]
    fn keeps_a_prefixed_path_that_does_not_name_the_target() {
        // `a/` could be a real directory; without a match against the accepted
        // location the prefix is kept and the mismatch surfaces downstream.
        let mut card = patch_card(file_patch("a/src/other.ts", HUNK));
        let context = context("src/work.ts", "old");

        PatchNormalizer::normalize_card(&mut card, &context).unwrap();

        let Card::Patch(card) = card else {
            unreachable!()
        };
        assert_eq!(card.patches[0].file, PathBuf::from("a/src/other.ts"));
    }

    #[test]
    fn keeps_a_prefixed_path_when_the_target_really_lives_under_it() {
        let mut card = patch_card(file_patch("a/src/work.ts", HUNK));
        let context = context("a/src/work.ts", "old");

        PatchNormalizer::normalize_card(&mut card, &context).unwrap();

        let Card::Patch(card) = card else {
            unreachable!()
        };
        assert_eq!(card.patches[0].file, PathBuf::from("a/src/work.ts"));
    }

    #[test]
    fn warns_about_an_incomplete_local_rename() {
        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename response binding".into(),
            explanation: "Rename response to rpc_response.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.rs"),
                diff: "@@ -1,2 +1,2 @@\n-let response = call();\n+let rpc_response = call();\n use_value(response);\n"
                    .into(),
                explanation: "Rename the local binding.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
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
        let mut card = Card::Patch(loopbiotic_protocol::PatchCard {
            id: "c_1".into(),
            title: "Rename response binding".into(),
            explanation: "Rename response to rpc_response.".into(),
            warnings: vec![],
            goal_complete: false,
            plan: None,
            patches: vec![FilePatch {
                id: "p_1".into(),
                file: PathBuf::from("src/work.rs"),
                diff: "@@ -1,2 +1,2 @@\n-let response = call();\n-use_value(response);\n+let rpc_response = call();\n+use_value(rpc_response);\n"
                    .into(),
                explanation: "Rename the binding and its use.".into(),
            }],
            file_ops: vec![],
            actions: vec![loopbiotic_protocol::Action::Apply],
        });

        PatchCoherence::annotate(&mut card);

        let Card::Patch(card) = card else {
            unreachable!();
        };
        assert!(card.warnings.is_empty());
    }
}
