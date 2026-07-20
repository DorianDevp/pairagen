mod cargo;
mod compose;
mod deno;
mod editor;
mod nx;
mod package;

use loopbiotic_protocol::{
    ProjectArea, ProjectCommand, ProjectSignals, ProjectTechnology, ProjectTool,
};

use super::facts::RootFacts;

#[derive(Default)]
pub(super) struct AdapterOutput {
    pub adapter: String,
    pub ecosystem: Option<String>,
    pub workspace_kind: Option<(u8, String)>,
    pub technologies: Vec<ProjectTechnology>,
    pub areas: Vec<ProjectArea>,
    pub commands: Vec<ProjectCommand>,
    pub tools: Vec<ProjectTool>,
}

pub(super) trait ProjectAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn root_files(&self) -> &'static [&'static str];
    fn matches(&self, facts: &RootFacts, signals: &ProjectSignals) -> bool;
    fn inspect(&self, facts: &RootFacts, signals: &ProjectSignals) -> AdapterOutput;
}

pub(super) fn builtins() -> Vec<Box<dyn ProjectAdapter>> {
    vec![
        Box::new(package::PackageWorkspaceAdapter),
        Box::new(package::PackageTechnologyAdapter::new(
            "typescript",
            "typescript",
            "TypeScript",
            "language",
        )),
        Box::new(package::PackageTechnologyAdapter::new(
            "angular",
            "@angular/core",
            "Angular",
            "web_framework",
        )),
        Box::new(package::PackageTechnologyAdapter::new(
            "react",
            "react",
            "React",
            "ui_library",
        )),
        Box::new(package::PackageTechnologyAdapter::new(
            "excalidraw",
            "@excalidraw/excalidraw",
            "Excalidraw",
            "editor_library",
        )),
        Box::new(package::PackageTechnologyAdapter::new(
            "rxjs",
            "rxjs",
            "RxJS",
            "reactive_library",
        )),
        Box::new(deno::DenoAdapter),
        Box::new(nx::NxAdapter),
        Box::new(cargo::CargoWorkspaceAdapter),
        Box::new(cargo::CargoTechnologyAdapter::new(
            "rust-axum",
            "axum",
            "Axum",
            "web_framework",
        )),
        Box::new(cargo::CargoTechnologyAdapter::new(
            "rust-sqlx",
            "sqlx",
            "SQLx",
            "database_client",
        )),
        Box::new(cargo::CargoTechnologyAdapter::new(
            "rust-tokio",
            "tokio",
            "Tokio",
            "async_runtime",
        )),
        Box::new(compose::ComposeAdapter),
        Box::new(compose::ComposeImageTechnologyAdapter::new(
            "postgres",
            "postgres:",
            "PostgreSQL",
            "database",
        )),
        Box::new(compose::ComposeImageTechnologyAdapter::new(
            "garage",
            "dxflrs/garage:v",
            "Garage",
            "object_storage",
        )),
        Box::new(editor::NeovimLspAdapter),
    ]
}
