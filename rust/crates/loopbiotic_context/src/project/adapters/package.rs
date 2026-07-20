use std::path::PathBuf;

use loopbiotic_protocol::{ProjectSignals, ProjectTechnology};

use super::{AdapterOutput, ProjectAdapter, RootFacts};

const ROOT_FILES: &[&str] = &["package.json", "deno.lock"];

pub(super) struct PackageWorkspaceAdapter;

impl ProjectAdapter for PackageWorkspaceAdapter {
    fn id(&self) -> &'static str {
        "package-workspace"
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        facts.has("package.json")
    }

    fn inspect(&self, _facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        AdapterOutput {
            ecosystem: Some("javascript".into()),
            workspace_kind: Some((10, "javascript_workspace".into())),
            ..AdapterOutput::default()
        }
    }
}

pub(super) struct PackageTechnologyAdapter {
    id: &'static str,
    package: &'static str,
    name: &'static str,
    role: &'static str,
}

impl PackageTechnologyAdapter {
    pub const fn new(
        id: &'static str,
        package: &'static str,
        name: &'static str,
        role: &'static str,
    ) -> Self {
        Self {
            id,
            package,
            name,
            role,
        }
    }
}

impl ProjectAdapter for PackageTechnologyAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn root_files(&self) -> &'static [&'static str] {
        ROOT_FILES
    }

    fn matches(&self, facts: &RootFacts, _signals: &ProjectSignals) -> bool {
        facts.package_dependency(self.package).is_some()
    }

    fn inspect(&self, facts: &RootFacts, _signals: &ProjectSignals) -> AdapterOutput {
        let (version, source) = facts
            .locked_npm_version(self.package)
            .map(str::to_string)
            .map(|version| (Some(version), "deno.lock"))
            .unwrap_or_else(|| {
                (
                    facts.package_dependency(self.package).map(str::to_string),
                    "package.json",
                )
            });
        AdapterOutput {
            technologies: vec![ProjectTechnology {
                name: self.name.into(),
                version,
                role: self.role.into(),
                source: PathBuf::from(source),
            }],
            ..AdapterOutput::default()
        }
    }
}
