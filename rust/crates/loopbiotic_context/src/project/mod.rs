mod adapters;
mod facts;

use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use loopbiotic_protocol::{
    ProjectArea, ProjectCommand, ProjectProfile, ProjectSignals, ProjectTechnology, ProjectTool,
};

use adapters::{AdapterOutput, ProjectAdapter};
use facts::RootFacts;

/// Backend-owned project inspection. Adapters declare their root markers and
/// are activated automatically; no adapter knows a project name.
#[derive(Default)]
pub struct ProjectProfiler;

impl ProjectProfiler {
    pub fn inspect(&self, root: &Path, signals: &ProjectSignals) -> ProjectProfile {
        let adapters = adapters::builtins();
        let root_files = adapters
            .iter()
            .flat_map(|adapter| adapter.root_files().iter().copied())
            .collect::<HashSet<_>>();
        let facts = RootFacts::load(root, root_files);
        let active = adapters
            .iter()
            .filter(|adapter| adapter.matches(&facts, signals))
            .map(Box::as_ref)
            .collect::<Vec<_>>();
        let outputs = inspect_parallel(&active, &facts, signals);
        merge(outputs)
    }
}

fn inspect_parallel(
    adapters: &[&dyn ProjectAdapter],
    facts: &RootFacts,
    signals: &ProjectSignals,
) -> Vec<AdapterOutput> {
    let (send, receive) = mpsc::channel();
    thread::scope(|scope| {
        for adapter in adapters {
            let send = send.clone();
            scope.spawn(move || {
                let mut output = adapter.inspect(facts, signals);
                output.adapter = adapter.id().into();
                let _ = send.send(output);
            });
        }
    });
    drop(send);
    receive.into_iter().collect()
}

fn merge(outputs: Vec<AdapterOutput>) -> ProjectProfile {
    let mut adapters = Vec::new();
    let mut technologies = Vec::<ProjectTechnology>::new();
    let mut areas = Vec::<ProjectArea>::new();
    let mut commands = Vec::<ProjectCommand>::new();
    let mut tools = Vec::<ProjectTool>::new();
    let mut ecosystems = HashSet::new();
    let mut kind = (0, "source_workspace".to_string());

    for output in outputs {
        adapters.push(output.adapter);
        technologies.extend(output.technologies);
        areas.extend(output.areas);
        commands.extend(output.commands);
        tools.extend(output.tools);
        if let Some(ecosystem) = output.ecosystem {
            ecosystems.insert(ecosystem);
        }
        if let Some(candidate) = output.workspace_kind
            && candidate.0 > kind.0
        {
            kind = candidate;
        }
    }
    if ecosystems.len() > 1 {
        kind = (u8::MAX, "polyglot_monorepo".into());
    }

    adapters.sort();
    technologies.sort_by(|left, right| (&left.name, &left.role).cmp(&(&right.name, &right.role)));
    technologies.dedup_by(|left, right| left.name == right.name && left.role == right.role);
    areas.sort_by(|left, right| left.path.cmp(&right.path));
    areas.dedup_by(|left, right| left.name == right.name && left.path == right.path);
    commands.sort_by(|left, right| (&left.name, &left.command).cmp(&(&right.name, &right.command)));
    commands.dedup_by(|left, right| left.name == right.name && left.command == right.command);
    tools.sort_by(|left, right| (&left.name, &left.source).cmp(&(&right.name, &right.source)));
    tools.dedup_by(|left, right| left.name == right.name && left.source == right.source);

    ProjectProfile {
        schema_version: 1,
        kind: kind.1,
        adapters,
        technologies,
        areas,
        commands,
        tools,
    }
}

#[cfg(test)]
mod tests;
