use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use loopbiotic_protocol::ContextPolicy;

use crate::rank::dependency_tokens;

const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    "target",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "coverage",
    "__pycache__",
    ".cache",
    ".angular",
    ".nx",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".gradle",
    ".dart_tool",
    ".yarn",
    ".pnpm-store",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".terraform",
];

/// Generated-tree exclusions that need more than one path segment: `var` or
/// `storage` alone are legitimate source directories, but their framework
/// cache subtrees (Symfony `var/cache`, Laravel `storage/framework`) are
/// generated code that must never be indexed, ranked, or matched by editor
/// hints.
const DEFAULT_EXCLUDED_PATHS: &[&str] = &[
    "var/cache",
    "var/log",
    "var/sessions",
    "storage/framework",
    "bootstrap/cache",
];

pub(crate) const SOURCE_EXTENSIONS: &[&str] = &[
    "rs",
    "lua",
    "py",
    "js",
    "jsx",
    "ts",
    "tsx",
    "go",
    "java",
    "kt",
    "kts",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "cs",
    "rb",
    "php",
    "swift",
    "scala",
    "ex",
    "exs",
    "erl",
    "hrl",
    "fs",
    "fsx",
    "clj",
    "cljs",
    "vue",
    "svelte",
    "sql",
    "sh",
    "bash",
    "zsh",
    "fish",
    "vim",
    "nix",
    "toml",
    "yaml",
    "yml",
    "json",
    "md",
    "mdx",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "xml",
    "njk",
    "jinja",
    "jinja2",
    "hbs",
    "handlebars",
    "tera",
    "twig",
    "ejs",
    "mustache",
    "astro",
    "graphql",
    "gql",
];

#[derive(Default)]
pub(crate) struct ProjectIndex {
    pub(crate) files: HashMap<PathBuf, IndexedFile>,
    pub(crate) last_refresh: Option<Instant>,
}

pub(crate) struct IndexedFile {
    modified_ns: u128,
    length: u64,
    pub(crate) lines: Vec<String>,
    pub(crate) lower_lines: Vec<String>,
    pub(crate) dependencies: Vec<String>,
}

pub(crate) struct RefreshStats {
    pub(crate) indexed_files: usize,
    pub(crate) hits: usize,
    pub(crate) misses: usize,
}

impl ProjectIndex {
    pub(crate) fn refresh(&mut self, root: &Path, policy: &ContextPolicy) -> RefreshStats {
        if self
            .last_refresh
            .is_some_and(|refresh| refresh.elapsed().as_millis() < policy.cache_ttl_ms as u128)
        {
            return RefreshStats {
                indexed_files: self.files.len(),
                hits: self.files.len(),
                misses: 0,
            };
        }
        let mut paths = Vec::new();
        collect_files(root, root, policy, &mut paths);
        paths.sort();
        paths.truncate(policy.max_scan_files);

        let mut seen = HashSet::new();
        let mut hits = 0;
        let mut misses = 0;

        for absolute in paths {
            let Ok(relative) = absolute.strip_prefix(root).map(Path::to_path_buf) else {
                continue;
            };
            let Ok(metadata) = fs::metadata(&absolute) else {
                continue;
            };
            let modified_ns = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let unchanged = self.files.get(&relative).is_some_and(|cached| {
                cached.modified_ns == modified_ns && cached.length == metadata.len()
            });

            seen.insert(relative.clone());
            if unchanged {
                hits += 1;
                continue;
            }

            let Ok(text) = fs::read_to_string(&absolute) else {
                continue;
            };
            if text.contains('\0') {
                continue;
            }
            let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
            let lower_lines = lines.iter().map(|line| line.to_lowercase()).collect();
            let dependencies = dependency_tokens(&text);
            self.files.insert(
                relative,
                IndexedFile {
                    modified_ns,
                    length: metadata.len(),
                    lines,
                    lower_lines,
                    dependencies,
                },
            );
            misses += 1;
        }

        self.files.retain(|path, _| seen.contains(path));
        self.last_refresh = Some(Instant::now());
        RefreshStats {
            indexed_files: self.files.len(),
            hits,
            misses,
        }
    }
}

fn collect_files(root: &Path, directory: &Path, policy: &ContextPolicy, files: &mut Vec<PathBuf>) {
    if files.len() >= policy.max_scan_files {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if files.len() >= policy.max_scan_files {
            break;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if excluded(relative, policy) {
            continue;
        }
        if file_type.is_dir() {
            collect_files(root, &path, policy, files);
        } else if file_type.is_file()
            && source_file(&path)
            && entry
                .metadata()
                .is_ok_and(|metadata| metadata.len() <= policy.max_file_bytes as u64)
        {
            files.push(path);
        }
    }
}

fn excluded(relative: &Path, policy: &ContextPolicy) -> bool {
    let components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| DEFAULT_EXCLUDED_DIRS.contains(component))
    {
        return true;
    }
    let normalized = relative.to_string_lossy().replace('\\', "/");
    let matches_pattern = |pattern: &str| {
        let pattern = pattern.trim_matches('/');
        !pattern.is_empty()
            && (normalized == pattern
                || normalized.starts_with(&format!("{pattern}/"))
                || normalized.contains(&format!("/{pattern}/")))
    };
    DEFAULT_EXCLUDED_PATHS
        .iter()
        .copied()
        .any(matches_pattern)
        || policy.exclude.iter().map(String::as_str).any(matches_pattern)
}

fn source_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if matches!(
        name,
        "Dockerfile" | "Makefile" | "Rakefile" | "Gemfile" | "Justfile" | "CMakeLists.txt"
    ) {
        return true;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SOURCE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use loopbiotic_protocol::ContextPolicy;

    use crate::ContextOptimizer;
    use crate::test_support::context;

    #[test]
    fn excludes_angular_build_cache_before_the_scan_limit() {
        let root =
            std::env::temp_dir().join(format!("loopbiotic-context-angular-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".angular/cache/21/babel-webpack")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join(".angular/cache/21/babel-webpack/generated.json"),
            r#"{"text":"preview_html preview_html preview_html"}"#,
        )
        .unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/preview.rs"), "pub fn preview_html() {}\n").unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "fn main() {}\n"),
            "Fix preview_html",
            &ContextPolicy {
                max_scan_files: 2,
                ..ContextPolicy::default()
            },
        );

        assert_eq!(optimized.report.as_ref().unwrap().indexed_files, 2);
        assert!(
            optimized
                .artifacts
                .iter()
                .all(|artifact| !artifact.file.starts_with(".angular"))
        );
        assert!(
            optimized
                .artifacts
                .iter()
                .any(|artifact| artifact.file == Path::new("src/preview.rs"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn excludes_framework_cache_subtrees_but_keeps_source_var_directories() {
        let root =
            std::env::temp_dir().join(format!("loopbiotic-context-symfony-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("var/cache/dev/ContainerAbc")).unwrap();
        fs::create_dir_all(root.join("src/var")).unwrap();
        fs::write(
            root.join("var/cache/dev/ContainerAbc/getMaker_FileManagerService.php"),
            "<?php function provide_invoices_cache() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/provider.php"),
            "<?php function provide_invoices() {}\n",
        )
        .unwrap();
        // `var` alone is a legitimate source directory name; only the
        // framework cache subtrees are generated code.
        fs::write(
            root.join("src/var/provide_invoices_helper.php"),
            "<?php function provide_invoices_helper() {}\n",
        )
        .unwrap();

        let mut optimizer = ContextOptimizer::default();
        let optimized = optimizer.optimize(
            context(&root, "<?php\n"),
            "Fix provide_invoices",
            &ContextPolicy::default(),
        );

        assert_eq!(optimized.report.as_ref().unwrap().indexed_files, 2);
        assert!(
            optimized
                .artifacts
                .iter()
                .all(|artifact| !artifact.file.starts_with("var"))
        );
        let _ = fs::remove_dir_all(root);
    }
}
