//! Live token-consumption harness.
//!
//! Drives the real `Engine` and a real backend (built from the environment,
//! exactly like the daemon) through a full pair-programming session for each
//! buggy TypeScript fixture, and reports how many goal steps, agent turns, and
//! tokens each configured model spent.
//!
//! This is a manual reporting tool, not a CI gate: it needs the real backend
//! CLIs/servers and credentials installed. Correctness is reported as
//! steps-taken + goal completion; there is no compiler or runtime gate, because
//! the fixtures deliberately mask type errors behind wrong `as` assertions that
//! a type checker would wave through.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use loopbiotic_harness::Engine;
use loopbiotic_patch::{PatchApply, UnifiedDiff};
use loopbiotic_protocol::{
    Action, Card, ContextBundle, Cursor, Diagnostic, GoalStatus, Mode, PatchApplyResult,
    StartSessionParams,
};
use serde::{Deserialize, Serialize};

/// Hard ceiling on backend round-trips per case so a stuck or looping model
/// cannot run forever. A hard fixture targets 6 steps; the cap leaves headroom
/// for retries, discovery cards, and clarifying replies.
const DEFAULT_MAX_TURNS: usize = 30;

/// The `case.json` next to every fixture's TypeScript files.
#[derive(Debug, Deserialize)]
struct CaseSpec {
    name: String,
    prompt: String,
    entry: PathBuf,
    #[serde(default)]
    cursor: Option<Cursor>,
    #[serde(default)]
    mode: Option<Mode>,
    #[serde(default)]
    target_steps: usize,
    #[serde(default)]
    diagnostics: Vec<Diagnostic>,
}

/// One (model x case) result row, also the JSONL record schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CaseReport {
    model: String,
    case: String,
    target_steps: usize,
    steps: usize,
    turns: usize,
    completed: bool,
    input: usize,
    cached: usize,
    output: usize,
    total: usize,
    estimated: bool,
    attempts: usize,
    elapsed_ms: u128,
    note: String,
}

/// Entry point for `loopbioticd dev token-report [flags]`.
pub async fn run(args: &[String]) -> Result<()> {
    let mut fixtures = PathBuf::from("tests/fixtures/token");
    let mut json_path: Option<PathBuf> = std::env::var_os("LOOPBIOTIC_REPORT_JSON").map(Into::into);
    let mut max_turns = DEFAULT_MAX_TURNS;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--render" => {
                let path = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--render needs a file"))?;
                return render(Path::new(path));
            }
            "--check" => {
                let base = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--check needs baseline"))?;
                let cur = args
                    .get(i + 2)
                    .ok_or_else(|| anyhow!("--check needs current file"))?;
                return check(Path::new(base), Path::new(cur));
            }
            "--fixtures" => {
                fixtures = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--fixtures needs a path"))?
                    .into();
                i += 1;
            }
            "--json" => {
                json_path = Some(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow!("--json needs a path"))?
                        .into(),
                );
                i += 1;
            }
            "--max-turns" => {
                max_turns = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--max-turns needs a number"))?
                    .parse()?;
                i += 1;
            }
            other => return Err(anyhow!("unknown token-report flag {other}")),
        }
        i += 1;
    }

    let model = model_label();
    let backend = crate::backend_from_env()?;

    let mut reports = Vec::new();
    for case_dir in fixture_dirs(&fixtures)? {
        let report = match run_case(&case_dir, &model, backend.clone(), max_turns).await {
            Ok(report) => report,
            Err(error) => {
                // A whole-case failure (unreadable fixture, backend refusing the
                // first turn) is still a row, not a crash of the whole run.
                let name = case_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                CaseReport {
                    model: model.clone(),
                    case: name,
                    note: format!("error: {error}"),
                    ..empty_report(&model)
                }
            }
        };
        reports.push(report);
    }

    if let Some(path) = &json_path {
        append_jsonl(path, &reports)?;
    }
    print_table(&reports);
    Ok(())
}

/// Runs a single fixture end-to-end against `backend` and returns its row.
async fn run_case(
    case_dir: &Path,
    model: &str,
    backend: Arc<dyn loopbiotic_backends::BackendAdapter>,
    max_turns: usize,
) -> Result<CaseReport> {
    let spec_raw = fs::read_to_string(case_dir.join("case.json"))
        .with_context(|| format!("reading {}/case.json", case_dir.display()))?;
    let spec: CaseSpec = serde_json::from_str(&spec_raw)
        .with_context(|| format!("parsing {}/case.json", case_dir.display()))?;

    // Work on a throwaway copy so a run never mutates the committed fixtures,
    // and so the agent cannot peek at case.json (which carries the difficulty
    // target and the intended prompt).
    let workdir = tempfile::Builder::new()
        .prefix(&format!("loopbiotic-token-{}-", spec.name))
        .tempdir()?;
    copy_fixture(case_dir, workdir.path())?;
    let cwd = workdir.path().to_path_buf();

    let entry_text = fs::read_to_string(cwd.join(&spec.entry))
        .with_context(|| format!("reading entry file {}", spec.entry.display()))?;

    let mut engine = Engine::new(backend);
    engine.set_source_context_provider(fs_context_provider(cwd.clone()));
    engine.set_location_granter(fs_location_granter(cwd.clone()));

    let params = StartSessionParams {
        cwd: cwd.clone(),
        file: spec.entry.clone(),
        cursor: spec.cursor.clone().unwrap_or(Cursor { line: 1, column: 1 }),
        selection: None,
        prompt: spec.prompt.clone(),
        mode: spec.mode.clone().unwrap_or(Mode::Auto),
        buffer_text: entry_text,
        buffer_start_line: 1,
        diagnostics: spec.diagnostics.clone(),
        hints: vec![],
        context_policy: Default::default(),
    };

    let started = Instant::now();
    let start = engine.start(params).await?;
    let session_id = start.session_id.clone();

    let mut card = start.card;
    let mut goal = start.goal;
    let mut total_usage = start.token_usage;
    let mut attempts = start.attempts.len();
    let mut turns = 1usize;
    let mut replies_used = 0usize;
    let mut note = String::new();

    while turns < max_turns {
        match &card {
            Card::Summary(_) => break,
            Card::Error(err) => {
                note = format!("error card: {}", first_line(&err.message));
                break;
            }
            Card::Patch(patch) => {
                let card_id = patch.id.clone();
                let (patch_ids, changed, context) = match apply_patch(&cwd, patch, &spec.entry) {
                    Ok(applied) => applied,
                    Err(error) => {
                        note = format!("patch apply failed: {error}");
                        break;
                    }
                };
                let result = engine
                    .apply_result(PatchApplyResult {
                        session_id: session_id.clone(),
                        card_id,
                        accepted: true,
                        patch_ids,
                        changed_files: changed,
                        error: None,
                        context,
                    })
                    .await?;
                turns += 1;
                attempts += result.attempts.len();
                total_usage = result.token_usage;
                goal = result.goal;
                card = result.card;
            }
            Card::Choice(_) => {
                if replies_used >= 2 {
                    note = "stopped: repeated clarifying questions".into();
                    break;
                }
                replies_used += 1;
                let result = engine
                    .reply(
                        &session_id,
                        "Use your best judgment and make the fix.".into(),
                    )
                    .await?;
                turns += 1;
                attempts += result.attempts.len();
                total_usage = result.token_usage;
                goal = result.goal;
                card = result.card;
            }
            other => {
                let Some(action) = drive_action(other.actions()) else {
                    note = format!("stopped: no drivable action on {:?} card", other.kind());
                    break;
                };
                let result = engine.action(&session_id, action).await?;
                turns += 1;
                attempts += result.attempts.len();
                total_usage = result.token_usage;
                goal = result.goal;
                card = result.card;
            }
        }
    }

    if turns >= max_turns && note.is_empty() {
        note = format!("hit turn cap ({max_turns})");
    }

    let completed = goal.status == GoalStatus::Complete;
    Ok(CaseReport {
        model: model.to_string(),
        case: spec.name,
        target_steps: spec.target_steps,
        steps: goal.completed_steps.len(),
        turns,
        completed,
        input: total_usage.input_tokens,
        cached: total_usage.cached_input_tokens,
        output: total_usage.output_tokens,
        total: total_usage.total_tokens,
        estimated: total_usage.estimated,
        attempts,
        elapsed_ms: started.elapsed().as_millis(),
        note,
    })
}

/// Applies every file patch in a card to the working copy and returns the ids,
/// changed files, and the updated context the editor would send back.
fn apply_patch(
    cwd: &Path,
    patch: &loopbiotic_protocol::PatchCard,
    entry: &Path,
) -> Result<(Vec<String>, Vec<PathBuf>, ContextBundle)> {
    let mut patch_ids = Vec::new();
    let mut changed = Vec::new();
    let mut primary: Option<PathBuf> = None;

    for file_patch in &patch.patches {
        let abs = cwd.join(&file_patch.file);
        let current = fs::read_to_string(&abs).unwrap_or_default();
        let diff = UnifiedDiff::parse(&file_patch.diff)
            .with_context(|| format!("parsing diff for {}", file_patch.file.display()))?;
        let updated = PatchApply::apply_to_text(&current, &diff)
            .with_context(|| format!("applying diff to {}", file_patch.file.display()))?;
        fs::write(&abs, &updated)?;
        patch_ids.push(file_patch.id.clone());
        changed.push(file_patch.file.clone());
        primary.get_or_insert_with(|| file_patch.file.clone());
    }

    let focus = primary.unwrap_or_else(|| entry.to_path_buf());
    let context = file_context(cwd, &focus)
        .ok_or_else(|| anyhow!("could not read patched file {}", focus.display()))?;
    Ok((patch_ids, changed, context))
}

/// Picks the action that best advances a goal from a discovery/deny card.
fn drive_action(actions: &[Action]) -> Option<Action> {
    const PRIORITY: &[Action] = &[
        Action::Fix,
        Action::Next,
        Action::Follow,
        Action::Open,
        Action::ResumeDraft,
        Action::RunCheck,
    ];
    for wanted in PRIORITY {
        if actions.iter().any(|a| a == wanted) {
            return Some(wanted.clone());
        }
    }
    None
}

/// Builds a context bundle for a file inside the working copy, mirroring what
/// the editor sends when it snapshots a buffer.
fn file_context(cwd: &Path, file: &Path) -> Option<ContextBundle> {
    let abs = if file.is_absolute() {
        file.to_path_buf()
    } else {
        cwd.join(file)
    };
    let text = fs::read_to_string(&abs).ok()?;
    let relative = abs
        .strip_prefix(cwd)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| file.to_path_buf());
    Some(ContextBundle {
        cwd: cwd.to_path_buf(),
        file: relative,
        cursor: Cursor { line: 1, column: 1 },
        selection: None,
        buffer_text: text,
        buffer_start_line: 1,
        diagnostics: vec![],
        hints: vec![],
        artifacts: vec![],
        report: None,
    })
}

fn fs_context_provider(cwd: PathBuf) -> loopbiotic_harness::SourceContextProvider {
    Arc::new(move |file, _session_id| {
        let cwd = cwd.clone();
        Box::pin(async move { file_context(&cwd, &file) })
    })
}

fn fs_location_granter(cwd: PathBuf) -> loopbiotic_harness::LocationGranter {
    Arc::new(move |card, _session_id| {
        let cwd = cwd.clone();
        Box::pin(async move { file_context(&cwd, &card.location.file) })
    })
}

/// Copies every fixture file except `case.json` into the working directory,
/// preserving subdirectories.
fn copy_fixture(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name == "case.json" {
            continue;
        }
        let target = dst.join(&name);
        if path.is_dir() {
            fs::create_dir_all(&target)?;
            copy_fixture(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Every immediate subdirectory of `fixtures` that has a `case.json`, sorted.
fn fixture_dirs(fixtures: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(fixtures)
        .with_context(|| format!("reading fixtures dir {}", fixtures.display()))?
    {
        let path = entry?.path();
        if path.is_dir() && path.join("case.json").exists() {
            dirs.push(path);
        }
    }
    dirs.sort();
    if dirs.is_empty() {
        return Err(anyhow!(
            "no case.json fixtures under {}",
            fixtures.display()
        ));
    }
    Ok(dirs)
}

/// A display label for the model under test: `LOOPBIOTIC_REPORT_MODEL` if the
/// caller set one, else the per-backend model env var, else the backend name.
fn model_label() -> String {
    if let Ok(label) = std::env::var("LOOPBIOTIC_REPORT_MODEL") {
        return label;
    }
    let backend = std::env::var("LOOPBIOTIC_BACKEND").unwrap_or_else(|_| "mock".into());
    let model = match backend.as_str() {
        "codex" | "codex_app" => std::env::var("LOOPBIOTIC_CODEX_MODEL").ok(),
        "claude" | "claude_app" => std::env::var("LOOPBIOTIC_CLAUDE_MODEL").ok(),
        "ollama" => std::env::var("LOOPBIOTIC_OLLAMA_MODEL").ok(),
        _ => None,
    };
    match model {
        Some(model) => format!("{backend}/{model}"),
        None => backend,
    }
}

fn empty_report(model: &str) -> CaseReport {
    CaseReport {
        model: model.to_string(),
        case: String::new(),
        target_steps: 0,
        steps: 0,
        turns: 0,
        completed: false,
        input: 0,
        cached: 0,
        output: 0,
        total: 0,
        estimated: false,
        attempts: 0,
        elapsed_ms: 0,
        note: String::new(),
    }
}

fn append_jsonl(path: &Path, reports: &[CaseReport]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for report in reports {
        writeln!(file, "{}", serde_json::to_string(report)?)?;
    }
    Ok(())
}

/// `--render <jsonl>`: print a unified table across every model in the file.
fn render(path: &Path) -> Result<()> {
    let reports = read_jsonl(path)?;
    print_table(&reports);
    Ok(())
}

fn read_jsonl(path: &Path) -> Result<Vec<CaseReport>> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut reports = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        reports.push(serde_json::from_str(line)?);
    }
    Ok(reports)
}

fn print_table(reports: &[CaseReport]) {
    println!(
        "{:<22} {:<16} {:>7} {:>5} {:>8} {:>7} {:>7} {:>8} {:>4} {:>3}  NOTE",
        "MODEL", "CASE", "STEPS", "TURNS", "IN", "OUT", "CACHED", "TOTAL", "ATT", "OK"
    );
    for r in reports {
        let steps = format!("{}/{}", r.steps, r.target_steps);
        let ok = if r.completed { "✓" } else { "·" };
        let total = if r.estimated {
            format!("~{}", r.total)
        } else {
            r.total.to_string()
        };
        println!(
            "{:<22} {:<16} {:>7} {:>5} {:>8} {:>7} {:>7} {:>8} {:>4} {:>3}  {}",
            truncate(&r.model, 22),
            truncate(&r.case, 16),
            steps,
            r.turns,
            r.input,
            r.output,
            r.cached,
            total,
            r.attempts,
            ok,
            r.note,
        );
    }
}

/// `--check <baseline> <current>`: flag completion regressions and token drift
/// beyond 15%. Exits non-zero if any regression is found.
fn check(baseline: &Path, current: &Path) -> Result<()> {
    const DRIFT: f64 = 0.15;
    let base = read_jsonl(baseline)?;
    let cur = read_jsonl(current)?;

    let mut regressions = 0usize;
    for c in &cur {
        let Some(b) = base.iter().find(|b| b.model == c.model && b.case == c.case) else {
            println!("NEW    {} {} (no baseline)", c.model, c.case);
            continue;
        };
        if b.completed && !c.completed {
            println!("REGRESS {} {}: completion lost", c.model, c.case);
            regressions += 1;
        }
        if b.total > 0 {
            let delta = (c.total as f64 - b.total as f64) / b.total as f64;
            if delta > DRIFT {
                println!(
                    "REGRESS {} {}: tokens {} -> {} (+{:.0}%)",
                    c.model,
                    c.case,
                    b.total,
                    c.total,
                    delta * 100.0
                );
                regressions += 1;
            }
        }
    }

    if regressions == 0 {
        println!("ok: no completion or token regressions vs baseline");
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        text.chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}
