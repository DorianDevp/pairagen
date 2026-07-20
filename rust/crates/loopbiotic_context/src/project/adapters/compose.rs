use std::path::PathBuf;

use loopbiotic_protocol::{ProjectSignals, ProjectTechnology};

use super::{AdapterOutput, ProjectAdapter, RootFacts};

const ROOT_FILES: &[&str] = &[
    "compose.yaml",
    "compose.yml",
    "docker-compose.yml",
    "docker-compose.yaml",
];

pub(super) struct ComposeAdapter;

impl ProjectAdapter for ComposeAdapter {
    fn id(&self) -> &'static str {
        "docker-compose"
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        compose(facts).is_some()
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        let Some((source, _)) = compose(facts) else {
            return AdapterOutput::default();
        };
        AdapterOutput {
            technologies: vec![ProjectTechnology {
                name: "Docker Compose".into(),
                version: None,
                role: "development_orchestration".into(),
                source: PathBuf::from(source),
            }],
            ..AdapterOutput::default()
        }
    }
}

pub(super) struct ComposeImageTechnologyAdapter {
    id: &'static str,
    image_prefix: &'static str,
    name: &'static str,
    role: &'static str,
}

impl ComposeImageTechnologyAdapter {
    pub const fn new(
        id: &'static str,
        image_prefix: &'static str,
        name: &'static str,
        role: &'static str,
    ) -> Self {
        Self {
            id,
            image_prefix,
            name,
            role,
        }
    }
}

impl ProjectAdapter for ComposeImageTechnologyAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        compose(facts)
            .and_then(|(_, content)| image_version(content, self.image_prefix))
            .is_some()
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        let Some((source, content)) = compose(facts) else {
            return AdapterOutput::default();
        };
        AdapterOutput {
            technologies: vec![ProjectTechnology {
                name: self.name.into(),
                version: image_version(content, self.image_prefix),
                role: self.role.into(),
                source: PathBuf::from(source),
            }],
            ..AdapterOutput::default()
        }
    }
}

fn compose(facts: &RootFacts) -> Option<(&'static str, &str)> {
    ROOT_FILES
        .iter()
        .find_map(|path| facts.text(path).map(|content| (*path, content)))
}

fn image_version(content: &str, prefix: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let image = line
            .trim()
            .strip_prefix("image:")?
            .trim()
            .trim_matches(['"', '\'']);
        let version = image.strip_prefix(prefix)?.split('@').next()?;
        (!version.is_empty()).then(|| version.to_string())
    })
}
