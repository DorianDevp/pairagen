use std::fs;

use loopbiotic_protocol::{ProjectLspClient, ProjectSignals, validate_project_metadata};

use super::ProjectProfiler;

fn write(root: &std::path::Path, path: &str, content: &str) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, content).unwrap();
}

fn technology<'a>(profile: &'a loopbiotic_protocol::ProjectProfile, name: &str) -> &'a str {
    profile
        .technologies
        .iter()
        .find(|technology| technology.name == name)
        .and_then(|technology| technology.version.as_deref())
        .unwrap()
}

#[test]
fn marker_registry_profiles_a_libregraf_shaped_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "package.json",
        r#"{"dependencies":{"@angular/core":"22.0.6","react":"18.3.1","@excalidraw/excalidraw":"0.18.1"},"devDependencies":{"typescript":"~6.0.0","nx":"23.1.0"}}"#,
    );
    write(
        root,
        "deno.lock",
        r#"{"specifiers":{"npm:@angular/core@22.0.6":"22.0.6_rxjs@7.8.2","npm:react@18.3.1":"18.3.1","npm:@excalidraw/excalidraw@0.18.1":"0.18.1_react@18.3.1","npm:nx@23.1.0":"23.1.0","npm:typescript@6.0":"6.0.3"}}"#,
    );
    write(
        root,
        "deno.json",
        r#"{"tasks":{"check":"nx run-many -t build","dev":"nx serve web-angular"}}"#,
    );
    write(root, "nx.json", "{}");
    write(
        root,
        "apps/web-angular/project.json",
        r#"{"name":"web-angular","sourceRoot":"apps/web-angular/src","projectType":"application","targets":{"build":{"executor":"@angular/build:application"}}}"#,
    );
    write(
        root,
        "apps/editor-react/project.json",
        r#"{"name":"editor-react","sourceRoot":"apps/editor-react/src","projectType":"library","implicitDependencies":["editor-contract"]}"#,
    );
    write(
        root,
        "Cargo.toml",
        r#"[workspace]
members = ["apps/api-rust", "crates/graph-model"]
[workspace.package]
edition = "2024"
[workspace.dependencies]
axum = "0.8.9"
sqlx = { version = "0.9.0", features = [] }
tokio = "1.49.0"
"#,
    );
    write(root, "deploy/web.Dockerfile", "FROM denoland/deno:2.9.0");
    write(
        root,
        "docker-compose.yml",
        "services:\n  db:\n    image: postgres:17-alpine\n  storage:\n    image: dxflrs/garage:v2.3.0\n",
    );
    let signals = ProjectSignals {
        lsp_clients: vec![ProjectLspClient {
            name: "angularls".into(),
            version: Some("22".into()),
            root: Some("apps/web-angular".into()),
            capabilities: vec!["definition".into(), "diagnostics".into()],
        }],
    };

    let profile = ProjectProfiler.inspect(root, &signals);

    assert_eq!(profile.kind, "polyglot_monorepo");
    assert_eq!(technology(&profile, "Angular"), "22.0.6");
    assert_eq!(technology(&profile, "TypeScript"), "6.0.3");
    assert_eq!(technology(&profile, "React"), "18.3.1");
    assert_eq!(technology(&profile, "Deno"), "2.9.0");
    assert_eq!(technology(&profile, "Rust"), "edition 2024");
    assert_eq!(technology(&profile, "PostgreSQL"), "17-alpine");
    assert_eq!(technology(&profile, "Garage"), "2.3.0");
    assert!(profile.adapters.contains(&"angular".into()));
    assert!(profile.adapters.contains(&"cargo-workspace".into()));
    assert!(profile.adapters.contains(&"neovim-lsp".into()));
    assert_eq!(profile.tools[0].name, "angularls");
    assert!(
        profile.areas.iter().any(|area| {
            area.name == "editor-react" && area.dependencies == ["editor-contract"]
        })
    );
}

#[test]
fn unrelated_root_does_not_activate_technology_adapters() {
    let temp = tempfile::tempdir().unwrap();
    let profile = ProjectProfiler.inspect(temp.path(), &ProjectSignals::default());
    assert_eq!(profile.kind, "source_workspace");
    assert!(profile.adapters.is_empty());
    assert!(profile.technologies.is_empty());
}

#[test]
fn go_module_reports_toolchain_dependencies_and_test_command() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "go.mod",
        r#"module example.com/service

go 1.24.0
toolchain go1.24.5

require (
    github.com/jackc/pgx/v5 v5.7.5
    golang.org/x/sync v0.15.0 // indirect
)
"#,
    );

    let profile = ProjectProfiler.inspect(root, &ProjectSignals::default());

    assert_eq!(profile.kind, "go_module");
    assert_eq!(profile.adapters, ["go-workspace"]);
    assert_eq!(technology(&profile, "Go"), "toolchain go1.24.5");
    assert_eq!(profile.areas.len(), 1);
    assert_eq!(profile.areas[0].name, "example.com/service");
    assert_eq!(profile.areas[0].path, std::path::Path::new("."));
    assert_eq!(
        profile.areas[0].dependencies,
        [
            "github.com/jackc/pgx/v5@v5.7.5",
            "golang.org/x/sync@v0.15.0"
        ]
    );
    assert!(
        profile
            .commands
            .iter()
            .any(|command| command.command == "go test ./...")
    );
    validate_project_metadata(Some(&profile), &[]).unwrap();
}

#[test]
fn go_work_reports_bounded_workspace_modules() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        "go.work",
        r#"go 1.24.0

use (
    ./services/api
    ./libs/model
    ../outside
)
"#,
    );
    write(
        root,
        "services/api/go.mod",
        "module example.com/api\n\ngo 1.24.0\nrequire example.com/model v0.0.0\n",
    );
    write(
        root,
        "libs/model/go.mod",
        "module example.com/model\n\ngo 1.24.0\n",
    );

    let profile = ProjectProfiler.inspect(root, &ProjectSignals::default());

    assert_eq!(profile.kind, "go_workspace");
    assert_eq!(technology(&profile, "Go"), "go 1.24.0");
    assert_eq!(profile.areas.len(), 2);
    assert_eq!(profile.areas[0].path, std::path::Path::new("libs/model"));
    assert_eq!(profile.areas[1].path, std::path::Path::new("services/api"));
    assert_eq!(profile.areas[1].dependencies, ["example.com/model@v0.0.0"]);
    assert_eq!(
        profile.commands[0].command,
        "go test ./libs/model/... ./services/api/..."
    );
    validate_project_metadata(Some(&profile), &[]).unwrap();
}

#[test]
fn go_and_javascript_mark_the_root_as_polyglot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, "go.mod", "module example.com/api\n\ngo 1.23.0\n");
    write(root, "package.json", r#"{"private":true}"#);

    let profile = ProjectProfiler.inspect(root, &ProjectSignals::default());

    assert_eq!(profile.kind, "polyglot_monorepo");
    assert!(profile.adapters.contains(&"go-workspace".into()));
    assert!(profile.adapters.contains(&"package-workspace".into()));
    validate_project_metadata(Some(&profile), &[]).unwrap();
}
