use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

const MAX_FACT_BYTES: u64 = 2 * 1024 * 1024;

pub(super) struct RootFacts {
    pub root: PathBuf,
    files: HashMap<&'static str, String>,
    package_dependencies: HashMap<String, String>,
    locked_npm_versions: HashMap<String, String>,
}

impl RootFacts {
    pub fn load(root: &Path, paths: HashSet<&'static str>) -> Self {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let (send, receive) = mpsc::channel();
        thread::scope(|scope| {
            for relative in paths {
                let send = send.clone();
                let root = &root;
                scope.spawn(move || {
                    if let Some(content) = read_bounded(root, Path::new(relative)) {
                        let _ = send.send((relative, content));
                    }
                });
            }
        });
        drop(send);
        let files = receive.into_iter().collect::<HashMap<_, _>>();
        let package_dependencies = parse_package_dependencies(files.get("package.json"));
        let locked_npm_versions = parse_locked_npm_versions(files.get("deno.lock"));
        Self {
            root,
            files,
            package_dependencies,
            locked_npm_versions,
        }
    }

    pub fn has(&self, relative: &str) -> bool {
        self.files.contains_key(relative)
    }

    pub fn text(&self, relative: &str) -> Option<&str> {
        self.files.get(relative).map(String::as_str)
    }

    pub fn json(&self, relative: &str) -> Option<serde_json::Value> {
        serde_json::from_str(self.text(relative)?).ok()
    }

    pub fn read(&self, relative: &Path) -> Option<String> {
        read_bounded(&self.root, relative)
    }

    pub fn package_dependency(&self, name: &str) -> Option<&str> {
        self.package_dependencies.get(name).map(String::as_str)
    }

    pub fn locked_npm_version(&self, name: &str) -> Option<&str> {
        self.locked_npm_versions.get(name).map(String::as_str)
    }
}

fn parse_package_dependencies(content: Option<&String>) -> HashMap<String, String> {
    let package =
        content.and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok());
    ["dependencies", "devDependencies", "peerDependencies"]
        .into_iter()
        .filter_map(|key| package.as_ref()?.get(key)?.as_object())
        .flat_map(|values| values.iter())
        .filter_map(|(name, version)| Some((name.clone(), version.as_str()?.to_string())))
        .collect()
}

fn parse_locked_npm_versions(content: Option<&String>) -> HashMap<String, String> {
    let Some(specifiers) = content
        .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .and_then(|lock| {
            lock.get("specifiers")
                .and_then(serde_json::Value::as_object)
                .cloned()
        })
    else {
        return HashMap::new();
    };
    specifiers
        .into_iter()
        .filter_map(|(specifier, resolved)| {
            let package_and_range = specifier.strip_prefix("npm:")?;
            let split = package_and_range.rfind('@')?;
            let version = resolved.as_str()?.split('_').next()?;
            Some((package_and_range[..split].to_string(), version.to_string()))
        })
        .collect()
}

fn read_bounded(root: &Path, relative: &Path) -> Option<String> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    let path = root.join(relative);
    let canonical = path.canonicalize().ok()?;
    if !canonical.starts_with(root) {
        return None;
    }
    let metadata = fs::metadata(&canonical).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FACT_BYTES {
        return None;
    }
    fs::read_to_string(canonical).ok()
}
