//! Controlled real-model A/B benchmark for Project Intelligence and Skills.
//! Both variants use the same current engine and backend; only the two features
//! introduced by the project-intelligence work are toggled.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use loopbiotic_harness::Engine;
use loopbiotic_protocol::{Card, Cursor, Diagnostic, InstructionSkill, Mode, StartSessionParams};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Variant {
    Before,
    Profile,
    After,
}

impl Variant {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "before" => Ok(Self::Before),
            "profile" => Ok(Self::Profile),
            "after" => Ok(Self::After),
            _ => Err(anyhow!("unknown A/B variant {value}")),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::Profile => "profile",
            Self::After => "after",
        }
    }

    fn profile(self) -> bool {
        !matches!(self, Self::Before)
    }

    fn skills(self) -> bool {
        matches!(self, Self::After)
    }
}

#[derive(Debug, Deserialize)]
struct CaseSpec {
    name: String,
    prompt: String,
    entry: PathBuf,
    mode: Mode,
    expected_kind: String,
    #[serde(default)]
    cursor: Option<Cursor>,
    #[serde(default)]
    diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    skills: Vec<PathBuf>,
    rubric: Vec<RubricItem>,
}

#[derive(Debug, Deserialize)]
struct RubricItem {
    label: String,
    #[serde(default)]
    any: Vec<String>,
    #[serde(default)]
    none: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AbReport {
    model: String,
    variant: String,
    case: String,
    run: usize,
    passed: bool,
    kind_ok: bool,
    score: usize,
    max_score: usize,
    rubric_score: usize,
    rubric_max: usize,
    card_kind: String,
    matched: Vec<String>,
    missed: Vec<String>,
    input: usize,
    cached: usize,
    output: usize,
    total: usize,
    estimated: bool,
    attempts: usize,
    outcomes: Vec<String>,
    violations: Vec<String>,
    elapsed_ms: u128,
    note: String,
    card: serde_json::Value,
}

pub async fn run(args: &[String]) -> Result<()> {
    let mut fixtures = PathBuf::from("tests/fixtures/project-intelligence");
    let mut variants = vec![Variant::Before, Variant::After];
    let mut selected_cases: Option<Vec<String>> = None;
    let mut repetitions = 1usize;
    let mut json_path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fixtures" => {
                fixtures = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--fixtures needs a path"))?
                    .into();
                i += 1;
            }
            "--variants" => {
                variants = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--variants needs a comma-separated list"))?
                    .split(',')
                    .map(Variant::parse)
                    .collect::<Result<Vec<_>>>()?;
                i += 1;
            }
            "--cases" => {
                selected_cases = Some(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow!("--cases needs a comma-separated list"))?
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                );
                i += 1;
            }
            "--repeat" => {
                repetitions = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--repeat needs a number"))?
                    .parse()?;
                i += 1;
            }
            "--json" => {
                json_path = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or_else(|| anyhow!("--json needs a path"))?,
                ));
                i += 1;
            }
            other => return Err(anyhow!("unknown ab-report flag {other}")),
        }
        i += 1;
    }
    if repetitions == 0 || variants.is_empty() {
        return Err(anyhow!("A/B benchmark needs at least one run and variant"));
    }

    let backend = crate::backend_from_env()?;
    let model = model_label();
    let mut cases = fixture_dirs(&fixtures)?;
    if let Some(selected) = selected_cases {
        cases.retain(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| selected.iter().any(|selected| selected == name))
        });
        if cases.is_empty() {
            return Err(anyhow!("none of the selected A/B cases exist"));
        }
    }
    let mut reports = Vec::new();
    for run in 1..=repetitions {
        for case in &cases {
            let order: Box<dyn Iterator<Item = &Variant>> = if run % 2 == 0 {
                Box::new(variants.iter().rev())
            } else {
                Box::new(variants.iter())
            };
            for variant in order {
                let report = run_case(case, &model, *variant, run, backend.clone()).await;
                reports.push(
                    report
                        .unwrap_or_else(|error| failed_report(&model, *variant, case, run, error)),
                );
            }
        }
    }

    if let Some(path) = json_path {
        append_jsonl(&path, &reports)?;
    }
    print_rows(&reports);
    print_summary(&reports);
    Ok(())
}

async fn run_case(
    case_dir: &Path,
    model: &str,
    variant: Variant,
    run: usize,
    backend: Arc<dyn loopbiotic_backends::BackendAdapter>,
) -> Result<AbReport> {
    let spec: CaseSpec = serde_json::from_str(&fs::read_to_string(case_dir.join("case.json"))?)?;
    let workdir = tempfile::Builder::new()
        .prefix(&format!("loopbiotic-ab-{}-{}-", spec.name, variant.label()))
        .tempdir()?;
    let excluded_skills = if variant.skills() {
        &[][..]
    } else {
        spec.skills.as_slice()
    };
    copy_fixture(case_dir, workdir.path(), excluded_skills)?;
    let entry_text = fs::read_to_string(workdir.path().join(&spec.entry))?;
    let skills = if variant.skills() {
        load_skills(workdir.path(), &spec.skills)?
    } else {
        vec![]
    };
    let params = StartSessionParams {
        cwd: workdir.path().to_path_buf(),
        file: spec.entry.clone(),
        cursor: spec.cursor.unwrap_or(Cursor { line: 1, column: 1 }),
        selection: None,
        prompt: spec.prompt,
        mode: spec.mode,
        buffer_text: entry_text,
        buffer_start_line: 1,
        diagnostics: spec.diagnostics,
        hints: vec![],
        call_hierarchy: None,
        context_policy: Default::default(),
        project_signals: Default::default(),
        skills,
    };
    let mut engine = Engine::new(backend);
    engine.set_project_intelligence(variant.profile());
    let started = Instant::now();
    let result = engine.start(params).await?;
    let elapsed_ms = started.elapsed().as_millis();
    let card_kind = format!("{:?}", result.card.kind()).to_lowercase();
    let card = serde_json::to_value(&result.card)?;
    let searchable = serde_json::to_string(&card)?.to_lowercase();
    let kind_ok = card_kind == spec.expected_kind.to_lowercase();
    let mut rubric_score = 0usize;
    let rubric_max = spec.rubric.len();
    let mut matched = Vec::new();
    let mut missed = Vec::new();
    for item in spec.rubric {
        let positive = item.any.is_empty()
            || item
                .any
                .iter()
                .any(|term| searchable.contains(&term.to_lowercase()));
        let negative = item
            .none
            .iter()
            .all(|term| !searchable.contains(&term.to_lowercase()));
        if positive && negative {
            rubric_score += 1;
            matched.push(item.label);
        } else {
            missed.push(item.label);
        }
    }
    Ok(AbReport {
        model: model.into(),
        variant: variant.label().into(),
        case: spec.name,
        run,
        passed: kind_ok && rubric_score == rubric_max,
        kind_ok,
        score: rubric_score + usize::from(kind_ok),
        max_score: rubric_max + 1,
        rubric_score,
        rubric_max,
        card_kind,
        matched,
        missed,
        input: result.token_usage.input_tokens,
        cached: result.token_usage.cached_input_tokens,
        output: result.token_usage.output_tokens,
        total: result.token_usage.total_tokens,
        estimated: result.token_usage.estimated,
        attempts: result.attempts.len(),
        outcomes: result
            .attempts
            .iter()
            .map(|attempt| attempt.outcome.clone())
            .collect(),
        violations: result
            .attempts
            .iter()
            .filter_map(|attempt| {
                attempt
                    .violation_class
                    .map(|class| format!("{class:?}").to_lowercase())
            })
            .collect(),
        elapsed_ms,
        note: card_note(&result.card),
        card,
    })
}

fn load_skills(root: &Path, paths: &[PathBuf]) -> Result<Vec<InstructionSkill>> {
    paths
        .iter()
        .map(|path| {
            let content = fs::read_to_string(root.join(path))
                .with_context(|| format!("reading benchmark skill {}", path.display()))?;
            Ok(InstructionSkill {
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path: path.clone(),
                content,
                provenance: "benchmark_fixture".into(),
                auto: path == Path::new("AGENTS.md"),
                sha256: "benchmark-fixture".into(),
            })
        })
        .collect()
}

fn card_note(card: &Card) -> String {
    match card {
        Card::Error(card) => first_line(&card.message).into(),
        Card::Deny(card) => first_line(&card.reason).into(),
        _ => String::new(),
    }
}

fn failed_report(
    model: &str,
    variant: Variant,
    case: &Path,
    run: usize,
    error: anyhow::Error,
) -> AbReport {
    AbReport {
        model: model.into(),
        variant: variant.label().into(),
        case: case
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        run,
        passed: false,
        kind_ok: false,
        score: 0,
        max_score: 0,
        rubric_score: 0,
        rubric_max: 0,
        card_kind: "error".into(),
        matched: vec![],
        missed: vec![],
        input: 0,
        cached: 0,
        output: 0,
        total: 0,
        estimated: false,
        attempts: 0,
        outcomes: vec![],
        violations: vec![],
        elapsed_ms: 0,
        note: format!("error: {error}"),
        card: serde_json::Value::Null,
    }
}

fn model_label() -> String {
    if let Ok(label) = std::env::var("LOOPBIOTIC_REPORT_MODEL") {
        return label;
    }
    let backend = std::env::var("LOOPBIOTIC_BACKEND").unwrap_or_else(|_| "mock".into());
    let model = match backend.as_str() {
        "codex" | "codex_app" => std::env::var("LOOPBIOTIC_CODEX_MODEL").ok(),
        "claude" | "claude_app" => std::env::var("LOOPBIOTIC_CLAUDE_MODEL").ok(),
        "ollama" => std::env::var("LOOPBIOTIC_OLLAMA_MODEL").ok(),
        "openai" | "openai_compatible" | "lm_studio" => {
            std::env::var("LOOPBIOTIC_OPENAI_MODEL").ok()
        }
        _ => None,
    };
    model
        .map(|model| format!("{backend}/{model}"))
        .unwrap_or(backend)
}

fn fixture_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("case.json").is_file())
        .collect::<Vec<_>>();
    result.sort();
    if result.is_empty() {
        return Err(anyhow!("no A/B fixtures under {}", root.display()));
    }
    Ok(result)
}

fn copy_fixture(source: &Path, target: &Path, excluded: &[PathBuf]) -> Result<()> {
    copy_fixture_tree(source, target, Path::new(""), excluded)
}

fn copy_fixture_tree(
    source: &Path,
    target: &Path,
    relative_root: &Path,
    excluded: &[PathBuf],
) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == "case.json" {
            continue;
        }
        let relative = relative_root.join(entry.file_name());
        if excluded.contains(&relative) {
            continue;
        }
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&destination)?;
            copy_fixture_tree(&entry.path(), &destination, &relative, excluded)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn append_jsonl(path: &Path, reports: &[AbReport]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
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

fn print_rows(reports: &[AbReport]) {
    println!(
        "{:<22} {:<8} {:<20} {:>7} {:>5} {:>8} {:>8} {:>4} {:>7}  MISSED",
        "MODEL", "VARIANT", "CASE", "SCORE", "OK", "TOKENS", "TIME ms", "ATT", "KIND"
    );
    for report in reports {
        println!(
            "{:<22} {:<8} {:<20} {:>3}/{:<3} {:>5} {:>8} {:>8} {:>4} {:>7}  {}",
            truncate(&report.model, 22),
            report.variant,
            truncate(&report.case, 20),
            report.score,
            report.max_score,
            if report.passed { "yes" } else { "no" },
            report.total,
            report.elapsed_ms,
            report.attempts,
            report.card_kind,
            report.missed.join(", "),
        );
    }
}

fn print_summary(reports: &[AbReport]) {
    #[derive(Default)]
    struct Aggregate {
        runs: usize,
        passed: usize,
        kind_ok: usize,
        rubric_score: usize,
        rubric_max: usize,
        tokens: usize,
        elapsed_ms: u128,
        attempts: usize,
    }
    let mut groups = BTreeMap::<(String, String), Aggregate>::new();
    for report in reports {
        let group = groups
            .entry((report.model.clone(), report.variant.clone()))
            .or_default();
        group.runs += 1;
        group.passed += usize::from(report.passed);
        group.kind_ok += usize::from(report.kind_ok);
        group.rubric_score += report.rubric_score;
        group.rubric_max += report.rubric_max;
        group.tokens += report.total;
        group.elapsed_ms += report.elapsed_ms;
        group.attempts += report.attempts;
    }
    println!("\nSUMMARY");
    for ((model, variant), group) in &groups {
        let content = percentage(group.rubric_score, group.rubric_max);
        let pass = percentage(group.passed, group.runs);
        let accepted = percentage(group.kind_ok, group.runs);
        println!(
            "{:<22} {:<8} pass {:>5.1}%  content {:>5.1}%  accepted {:>5.1}%  avg tokens {:>7.0}  avg time {:>7.0} ms  avg attempts {:.1}",
            truncate(model, 22),
            variant,
            pass,
            content,
            accepted,
            group.tokens as f64 / group.runs as f64,
            group.elapsed_ms as f64 / group.runs as f64,
            group.attempts as f64 / group.runs as f64,
        );
    }
    let models = reports
        .iter()
        .map(|report| report.model.clone())
        .collect::<std::collections::BTreeSet<_>>();
    println!("\nDELTA before -> after");
    for model in models {
        let before = groups.get(&(model.clone(), "before".into()));
        let after = groups.get(&(model.clone(), "after".into()));
        if let (Some(before), Some(after)) = (before, after) {
            let quality_delta = percentage(after.rubric_score, after.rubric_max)
                - percentage(before.rubric_score, before.rubric_max);
            let token_delta = relative_delta(before.tokens, before.runs, after.tokens, after.runs);
            let time_delta =
                relative_delta_u128(before.elapsed_ms, before.runs, after.elapsed_ms, after.runs);
            println!(
                "{:<22} quality {:+.1} pp  tokens {:+.1}%  time {:+.1}%",
                truncate(&model, 22),
                quality_delta,
                token_delta,
                time_delta,
            );
        }
    }
}

fn percentage(value: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        value as f64 * 100.0 / total as f64
    }
}

fn relative_delta(before: usize, before_n: usize, after: usize, after_n: usize) -> f64 {
    let before = before as f64 / before_n as f64;
    let after = after as f64 / after_n as f64;
    if before == 0.0 {
        0.0
    } else {
        (after - before) * 100.0 / before
    }
}

fn relative_delta_u128(before: u128, before_n: usize, after: u128, after_n: usize) -> f64 {
    relative_delta(before as usize, before_n, after as usize, after_n)
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        value.into()
    } else {
        value.chars().take(width - 1).collect::<String>() + "…"
    }
}

fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_copy_hides_skills_from_disabled_variants() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(source.path().join("AGENTS.md"), "secret instructions").unwrap();
        fs::write(source.path().join("source.ts"), "export {};").unwrap();

        copy_fixture(source.path(), target.path(), &[PathBuf::from("AGENTS.md")]).unwrap();

        assert!(!target.path().join("AGENTS.md").exists());
        assert!(target.path().join("source.ts").exists());
    }

    #[test]
    fn percentage_handles_empty_and_nonempty_samples() {
        assert_eq!(percentage(0, 0), 0.0);
        assert_eq!(percentage(2, 4), 50.0);
    }
}
