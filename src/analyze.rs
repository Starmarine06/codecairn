//! High-level analysis: file collection, manifest parsing, structure grouping,
//! and building the import graph.
//!
//! Import resolution works against a registry (`import_to_file`) built over
//! three kinds of keys, layered deterministically first-wins:
//!   1. identity and file-stem keys (all languages),
//!   2. dotted Python module keys for files inside `__init__.py` packages,
//!   3. `crate::<module path>` keys for Rust files with a known crate root.
//!
//! Files are sorted by path before registration so ambiguous stem/dotted keys
//! always resolve to the same file regardless of walk order.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::Result;
use ignore::WalkBuilder;

use crate::extract::extract_symbols_imports_purpose;
use crate::format::guess_directory_role;
use crate::resolve::{resolve_import, rust_file_crate_root, rust_module_path};
use crate::{DepGraph, DirectoryInfo, FileInfo, Language, ManifestInfo};

pub(crate) fn collect_files(root: &Path, include_hidden: bool) -> Result<Vec<FileInfo>> {
    let mut files = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(!include_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        let entry = entry?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let rel_path = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if should_skip(&rel_path) {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = Language::from_extension(ext);

        if language == Language::Other {
            continue;
        }

        let content = fs::read_to_string(path).unwrap_or_default();
        let lines = content.lines().count();

        let (symbols, imports, purpose) = extract_symbols_imports_purpose(&content, language);

        files.push(FileInfo {
            path: rel_path,
            language,
            lines,
            symbols,
            imports,
            purpose,
        });
    }

    Ok(files)
}

pub(crate) fn should_skip(path: &str) -> bool {
    let skip_dirs = [
        "target",
        "node_modules",
        ".git",
        "__pycache__",
        ".pytest_cache",
        "dist",
        "build",
        ".venv",
        "venv",
        "env",
        ".env",
        "coverage",
        ".next",
        ".nuxt",
        ".turbo",
        ".vercel",
        "vendor",
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
    ];

    for part in path.split(['/', '\\']) {
        if skip_dirs.contains(&part) {
            return true;
        }
        if part.starts_with('.') && part != "." && part != ".." {
            return true;
        }
    }
    false
}

pub(crate) fn collect_manifests(root: &Path) -> Result<Vec<ManifestInfo>> {
    let mut manifests = Vec::new();

    // package.json
    let pkg_path = root.join("package.json");
    if pkg_path.exists() {
        if let Ok(content) = fs::read_to_string(&pkg_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                let deps = json["dependencies"]
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                let dev_deps = json["devDependencies"]
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                let scripts = json["scripts"]
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                manifests.push(ManifestInfo {
                    name: json["name"].as_str().map(|s| s.to_string()),
                    description: json["description"].as_str().map(|s| s.to_string()),
                    dependencies: deps,
                    dev_dependencies: dev_deps,
                    scripts,
                });
            }
        }
    }

    // Cargo.toml
    let cargo_path = root.join("Cargo.toml");
    if cargo_path.exists() {
        if let Ok(content) = fs::read_to_string(&cargo_path) {
            if let Ok(toml) = content.parse::<toml::Value>() {
                let deps = toml
                    .get("dependencies")
                    .and_then(|v| v.as_table())
                    .map(|t| {
                        t.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();

                let dev_deps = toml
                    .get("dev-dependencies")
                    .and_then(|v| v.as_table())
                    .map(|t| {
                        t.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();

                manifests.push(ManifestInfo {
                    name: toml
                        .get("package")
                        .and_then(|p| p.get("name"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    description: toml
                        .get("package")
                        .and_then(|p| p.get("description"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    dependencies: deps,
                    dev_dependencies: dev_deps,
                    scripts: BTreeMap::new(),
                });
            }
        }
    }

    // pyproject.toml / setup.py / requirements.txt
    let pyproject_path = root.join("pyproject.toml");
    if pyproject_path.exists() {
        if let Ok(content) = fs::read_to_string(&pyproject_path) {
            if let Ok(toml) = content.parse::<toml::Value>() {
                let deps = toml
                    .get("project")
                    .and_then(|p| p.get("dependencies"))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .enumerate()
                            .map(|(i, s)| (format!("dep{}", i), s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                manifests.push(ManifestInfo {
                    name: toml
                        .get("project")
                        .and_then(|p| p.get("name"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    description: toml
                        .get("project")
                        .and_then(|p| p.get("description"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    dependencies: deps,
                    dev_dependencies: BTreeMap::new(),
                    scripts: BTreeMap::new(),
                });
            }
        }
    }

    Ok(manifests)
}

pub(crate) fn find_entry_points(root: &Path, files: &[FileInfo]) -> Result<Vec<String>> {
    let mut entry_points = Vec::new();

    // Common entry point patterns
    let patterns = [
        "main.py",
        "app.py",
        "__main__.py",
        "run.py",
        "server.py",
        "main.rs",
        "lib.rs",
        "index.ts",
        "main.ts",
        "app.ts",
        "server.ts",
        "index.js",
        "main.js",
        "app.js",
        "server.js",
    ];

    for pattern in &patterns {
        let path = root.join(pattern);
        if path.exists() {
            entry_points.push(pattern.to_string());
        }
    }

    // Also look for files with `if __name__ == "__main__"` or similar
    for file in files {
        if file.language == Language::Python {
            let full_path = root.join(&file.path);
            if let Ok(content) = fs::read_to_string(&full_path) {
                if content.contains("if __name__ == \"__main__\"")
                    || content.contains("if __name__ == '__main__'")
                {
                    entry_points.push(file.path.clone());
                }
            }
        }
    }

    Ok(entry_points)
}

pub(crate) fn count_tests(files: &[FileInfo]) -> usize {
    files
        .iter()
        .filter(|f| {
            f.path.contains("test") || f.path.contains("spec") || f.path.contains("__tests__")
        })
        .count()
}

pub(crate) fn extract_readme_summary(root: &Path) -> Option<String> {
    let patterns = ["README.md", "README.txt", "README.rst", "README"];
    for pattern in &patterns {
        let path = root.join(pattern);
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                // Get first paragraph (non-empty lines until blank line)
                let mut summary = String::new();
                let mut in_paragraph = false;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('#') {
                        if !in_paragraph {
                            in_paragraph = true;
                        }
                        if !summary.is_empty() {
                            summary.push(' ');
                        }
                        summary.push_str(trimmed);
                    } else if in_paragraph {
                        break;
                    }
                }
                if !summary.is_empty() {
                    return Some(summary);
                }
            }
        }
    }
    None
}

pub(crate) fn count_languages(files: &[FileInfo]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for file in files {
        *counts.entry(file.language.name().to_string()).or_insert(0) += file.lines;
    }
    counts
}

fn register(map: &mut HashMap<String, String>, key: &str, value: &str) {
    // First registration wins so a deterministic file order decides collisions.
    map.entry(key.to_string())
        .or_insert_with(|| value.to_string());
}

fn file_stem(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let stem = name
        .strip_suffix(".py")
        .or_else(|| name.strip_suffix(".rs"))
        .or_else(|| name.strip_suffix(".tsx"))
        .or_else(|| name.strip_suffix(".ts"))
        .or_else(|| name.strip_suffix(".jsx"))
        .or_else(|| name.strip_suffix(".js"))
        .or_else(|| name.strip_suffix(".mjs"))
        .or_else(|| name.strip_suffix(".cjs"))
        .unwrap_or(name);
    Some(stem.to_string())
}

fn is_package_dir(dir: &str, files: &HashSet<String>) -> bool {
    files.contains(&format!("{}/__init__.py", dir))
}

/// Dotted importable keys for a `.py` file, e.g. `app/models.py` inside the
/// `app` package yields `app.models`; `app/views/__init__.py` yields
/// `app.views`. Nested package chains register keys from every valid root, so
/// both the full and the local dotted name resolve.
fn python_module_keys(file: &str, files: &HashSet<String>) -> Vec<String> {
    let mut keys = Vec::new();
    if !file.ends_with(".py") {
        return keys;
    }
    let parts: Vec<&str> = file.split('/').collect();
    if parts.len() < 2 {
        return keys;
    }
    let dir_parts = &parts[..parts.len() - 1];
    let name = parts[parts.len() - 1];
    let is_init = name == "__init__.py";
    let stem = name.strip_suffix(".py").unwrap_or(name);

    for i in 0..dir_parts.len() {
        let mut chain_ok = true;
        for k in i..dir_parts.len() {
            if !is_package_dir(&dir_parts[..=k].join("/"), files) {
                chain_ok = false;
                break;
            }
        }
        if !chain_ok {
            continue;
        }
        let base = dir_parts[i..].join(".");
        if is_init {
            keys.push(base);
        } else {
            keys.push(format!("{}.{}", base, stem));
        }
    }
    keys
}

fn build_import_registry(files: &[FileInfo]) -> HashMap<String, String> {
    let mut registry: HashMap<String, String> = HashMap::new();
    let file_set: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();

    let mut ordered: Vec<&FileInfo> = files.iter().collect();
    ordered.sort_by(|a, b| a.path.cmp(&b.path));

    for file in ordered {
        // 1. Identity + stem.
        register(&mut registry, &file.path, &file.path);
        if let Some(stem) = file_stem(&file.path) {
            register(&mut registry, &stem, &file.path);
        }

        // 2. Python package chains.
        if file.language == Language::Python && file.path.ends_with(".py") {
            for key in python_module_keys(&file.path, &file_set) {
                register(&mut registry, &key, &file.path);
            }
        }

        // 3. Rust crate-relative module keys.
        if file.language == Language::Rust {
            if let Some(crate_root) = rust_file_crate_root(&file.path) {
                let module_path = rust_module_path(&file.path, &crate_root).unwrap_or_default();
                let key = if module_path.is_empty() {
                    "crate".to_string()
                } else {
                    format!("crate::{}", module_path.join("::"))
                };
                register(&mut registry, &key, &file.path);
            }
        }
    }

    registry
}

pub(crate) fn analyze_structure(
    _root: &Path,
    files: &[FileInfo],
) -> Result<(Vec<DirectoryInfo>, DepGraph)> {
    let mut dir_map: HashMap<String, Vec<&FileInfo>> = HashMap::new();

    // Group files by directory.
    for file in files {
        let dir = Path::new(&file.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        dir_map.entry(dir).or_default().push(file);
    }

    let import_to_file = build_import_registry(files);

    // Resolve imports to local files.
    let mut dep_graph: DepGraph = HashMap::new();
    for file in files {
        let crate_root = if file.language == Language::Rust {
            rust_file_crate_root(&file.path)
        } else {
            None
        };

        let mut resolved_deps = Vec::new();
        for import in &file.imports {
            if let Some(resolved) = resolve_import(
                import,
                &file.path,
                file.language,
                &import_to_file,
                crate_root.as_deref(),
            ) {
                if resolved != file.path {
                    resolved_deps.push(resolved);
                }
            }
        }
        resolved_deps.sort();
        resolved_deps.dedup();
        dep_graph.insert(file.path.clone(), resolved_deps);
    }

    // Create directory infos with guessed roles.
    let mut dir_infos = Vec::new();
    for (dir, dir_files) in &dir_map {
        let role = guess_directory_role(dir, dir_files);
        let mut file_names: Vec<String> = dir_files.iter().map(|f| f.path.clone()).collect();
        file_names.sort();
        dir_infos.push(DirectoryInfo {
            path: dir.clone(),
            role,
            file_count: dir_files.len(),
            files: file_names,
        });
    }

    dir_infos.sort_by(|a, b| a.path.cmp(&b.path));

    Ok((dir_infos, dep_graph))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(root: &Path, rel: &str, content: &str) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, content).unwrap();
    }

    fn make_file(path: &str, lang: Language, symbols: &[&str], import_s: &[&str]) -> FileInfo {
        FileInfo {
            path: path.to_string(),
            language: lang,
            lines: 10,
            symbols: symbols.iter().map(|s| s.to_string()).collect(),
            imports: import_s.iter().map(|s| s.to_string()).collect(),
            purpose: None,
        }
    }

    #[test]
    fn should_skip_ignores_build_dirs_and_hidden() {
        assert!(should_skip("node_modules/pkg/index.js"));
        assert!(should_skip("target/release/codecairn"));
        assert!(should_skip("src/.secret.py"));
        assert!(should_skip(".venv/lib/python3/site-packages/x.py"));
        assert!(should_skip("Cargo.lock"));
        assert!(should_skip("package-lock.json"));
        assert!(!should_skip("src/main.rs"));
        assert!(!should_skip("tests/test_app.py"));
    }

    #[test]
    fn python_module_keys_covers_nested_packages() {
        let files = [
            "app/__init__.py",
            "app/main.py",
            "app/views/__init__.py",
            "app/views/home.py",
        ];
        let set: HashSet<String> = files.iter().map(|s| s.to_string()).collect();

        assert_eq!(python_module_keys("app/main.py", &set), vec!["app.main"]);
        // Newest package __init__ registers the package name plus any enclosing
        // valid chain, from every valid root.
        let views = python_module_keys("app/views/__init__.py", &set);
        assert!(views.contains(&"app.views".to_string()));
        assert!(views.contains(&"views".to_string()));
        assert!(!views.contains(&"home".to_string()));
    }

    #[test]
    fn manifests_parse_python_and_cargo() {
        let dir = std::env::temp_dir().join(format!("codecairn-mfx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        write_fixture(
            &dir,
            "package.json",
            r#"{"name":"demo-pkg","description":"demo pkg","dependencies":{"express":"^4"},"devDependencies":{"jest":"^29"},"scripts":{"test":"jest"}}"#,
        );
        write_fixture(
            &dir,
            "Cargo.toml",
            "[package]\nname = \"demo\"\ndescription = \"demo cargo\"\n[dependencies]\nserde = \"1\"\n",
        );

        let manifests = collect_manifests(&dir).unwrap();
        assert_eq!(manifests.len(), 2);

        let pkg = manifests.iter().find(|m| m.scripts.contains_key("test"));
        if let Some(m) = pkg {
            assert_eq!(
                m.dependencies.get("express").map(String::as_str),
                Some("^4")
            );
            assert_eq!(
                m.dev_dependencies.get("jest").map(String::as_str),
                Some("^29")
            );
        } else {
            panic!("package.json manifest not found");
        }

        let cargo = manifests.iter().find(|m| m.name.as_deref() == Some("demo"));
        if let Some(m) = cargo {
            assert_eq!(m.dependencies.get("serde").map(String::as_str), Some("1"));
            assert!(m.scripts.is_empty());
        } else {
            panic!("Cargo.toml manifest not found");
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_points_and_counts_are_consistent() {
        let dir = std::env::temp_dir().join(format!("codecairn-ep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_fixture(&dir, "src/main.py", "def main():\n    pass\n");
        write_fixture(
            &dir,
            "src/cli.py",
            "if __name__ == \"__main__\":\n    main()\n",
        );
        write_fixture(&dir, "tests/test_app.py", "def test_app():\n    pass\n");
        write_fixture(&dir, "app/spec/helper.spec.ts", "export const x = 1;\n");

        let files = collect_files(&dir, false).unwrap();
        let entry_points = find_entry_points(&dir, &files).unwrap();
        assert!(entry_points.iter().any(|e| e == "src/cli.py"));
        assert_eq!(count_tests(&files), 2);
        let counts = count_languages(&files);
        assert!(counts.get("Python").copied().unwrap_or(0) >= 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn readme_summary_skips_headings() {
        let dir = std::env::temp_dir().join(format!("codecairn-rd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_fixture(
            &dir,
            "README.md",
            "# My API\n\nA tiny REST API.\n\nSecond paragraph ignored.\n",
        );
        assert_eq!(
            extract_readme_summary(&dir).as_deref(),
            Some("A tiny REST API.")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn golden_fixture_generates_expected_graph() {
        let dir = std::env::temp_dir().join(format!("codecairn-gold-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        write_fixture(
            &dir,
            "README.md",
            "# demo\n\nA small demo project used for tests.\n",
        );
        write_fixture(
            &dir,
            "package.json",
            r#"{"name":"demo","description":"demo","dependencies":{"express":"^4"},"scripts":{"test":"jest"}}"#,
        );
        write_fixture(
            &dir,
            "src/main.py",
            "\"\"\"Entry point.\"\"\"\nfrom . import helpers\ndef main():\n    pass\n",
        );
        write_fixture(&dir, "src/__init__.py", "\"\"\"Package root.\"\"\"\n");
        write_fixture(
            &dir,
            "src/helpers.py",
            "\"\"\"Helper utilities.\"\"\"\nclass Helper:\n    pass\n",
        );
        write_fixture(&dir, "tests/test_main.py", "def test_main():\n    pass\n");
        write_fixture(
            &dir,
            "src/main.ts",
            "// TS entry\nimport { Helper } from './helpers';\nexport function tsMain() {}\n",
        );

        let files = collect_files(&dir, false).unwrap();
        let manifests = collect_manifests(&dir).unwrap();
        let test_count = count_tests(&files);
        let (dirs, dep_graph) = analyze_structure(&dir, &files).unwrap();

        assert!(files.iter().any(|f| f.path.contains("main.py")));
        assert!(!files.iter().any(|f| f.path.contains("node_modules")));
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name.as_deref(), Some("demo"));
        assert!(test_count >= 1);

        // helpers.py should be imported by main.py, so it appears in the graph.
        let helper_deps = dep_graph.get("src/helpers.py").cloned().unwrap_or_default();
        let main_deps = dep_graph.get("src/main.py").cloned().unwrap_or_default();
        assert!(main_deps.iter().any(|d| d.contains("helpers")));
        assert!(helper_deps.is_empty());

        // The TS file imports ./helpers but lives in the same directory, so it
        // should resolve to src/helpers.ts — which does not exist. Ensure the
        // unresolved import stays out of the graph (separation is by file).
        let ts_deps = dep_graph.get("src/main.ts").cloned().unwrap_or_default();
        assert!(!ts_deps.iter().any(|d| d.contains("helpers")));

        // Directory roles are guessed from paths.
        assert!(dirs.iter().any(|d| d.path == "tests" && d.role == "tests"));
        assert!(
            dirs.iter().any(|d| d.path == "src" && d.role == "root")
                || dirs.iter().any(|d| d.path == "src")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependency_graph_is_relaxed_and_deduped() {
        let files = vec![
            make_file(
                "src/a.rs",
                Language::Rust,
                &["a"],
                &["crate::b", "crate::b"],
            ),
            make_file("src/b.rs", Language::Rust, &["b"], &[]),
            make_file("src/lib.rs", Language::Rust, &["lib"], &["self::a"]),
            make_file(
                "src/self_ref.rs",
                Language::Rust,
                &["s"],
                &["crate::self_ref"],
            ),
        ];
        let (_, dep_graph) = analyze_structure(Path::new("."), &files).unwrap();
        // Duplicates collapsed; self import excluded.
        assert_eq!(
            dep_graph.get("src/a.rs").cloned().unwrap_or_default(),
            vec!["src/b.rs".to_string()]
        );
        assert!(dep_graph
            .get("src/self_ref.rs")
            .unwrap_or(&Vec::new())
            .is_empty());
    }
}
