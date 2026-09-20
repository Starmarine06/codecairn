// Markdown formatting, directory role guessing, and key-file ranking.
// Moved out of main.rs during the monolith split.

use std::collections::HashMap;

use crate::{DepGraph, FileInfo, KeyFileInfo, Language, ProjectMap};

pub(crate) fn guess_directory_role(dir: &str, files: &[&FileInfo]) -> String {
    let dir_lower = dir.to_lowercase();
    let file_names: Vec<String> = files.iter().map(|f| f.path.to_lowercase()).collect();
    let _all_content: String = files
        .iter()
        .map(|f| f.path.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    if dir_lower.contains("test")
        || file_names
            .iter()
            .any(|f| f.contains("test") || f.contains("spec"))
    {
        return "tests".to_string();
    }
    if dir_lower.contains("route")
        || dir_lower.contains("controller")
        || dir_lower.contains("handler")
    {
        return "routes".to_string();
    }
    if dir_lower.contains("model") || dir_lower.contains("entity") || dir_lower.contains("schema") {
        return "models".to_string();
    }
    if dir_lower.contains("view")
        || dir_lower.contains("template")
        || dir_lower.contains("component")
    {
        return "views".to_string();
    }
    if dir_lower.contains("service")
        || dir_lower.contains("util")
        || dir_lower.contains("helper")
        || dir_lower.contains("lib")
    {
        return "services".to_string();
    }
    if dir_lower.contains("middleware") {
        return "middleware".to_string();
    }
    if dir_lower.contains("config") || dir_lower.contains("setting") {
        return "config".to_string();
    }
    if dir_lower.contains("migration") || dir_lower.contains("seed") {
        return "migrations".to_string();
    }
    if dir_lower == "." || dir_lower.is_empty() {
        return "root".to_string();
    }

    // Check file extensions to guess
    let has_py = files.iter().any(|f| f.language == Language::Python);
    let has_ts = files.iter().any(|f| f.language == Language::TypeScript);
    let has_rs = files.iter().any(|f| f.language == Language::Rust);

    if has_py && !has_ts && !has_rs {
        return "python".to_string();
    }
    if has_ts && !has_py && !has_rs {
        return "typescript".to_string();
    }
    if has_rs && !has_py && !has_ts {
        return "rust".to_string();
    }

    "other".to_string()
}

pub(crate) fn rank_key_files(
    files: &[FileInfo],
    dep_graph: &DepGraph,
    max: usize,
) -> Vec<KeyFileInfo> {
    // Count dependents (reverse dependency graph)
    let mut dependent_count: HashMap<String, usize> = HashMap::new();
    for deps in dep_graph.values() {
        for dep in deps {
            *dependent_count.entry(dep.clone()).or_insert(0) += 1;
        }
    }

    // Build file lookup (unused but kept for potential future use)
    let _file_map: HashMap<String, &FileInfo> = files.iter().map(|f| (f.path.clone(), f)).collect();

    // Create key file entries
    let mut key_files: Vec<KeyFileInfo> = files
        .iter()
        .map(|f| {
            let deps = dep_graph.get(&f.path).cloned().unwrap_or_default();
            let count = dependent_count.get(&f.path).copied().unwrap_or(0);
            KeyFileInfo {
                path: f.path.clone(),
                purpose: f.purpose.clone(),
                symbols: f.symbols.clone(),
                dependencies: deps,
                dependent_count: count,
            }
        })
        .collect();

    // Sort by dependent count (descending), then by path
    key_files.sort_by(|a, b| {
        b.dependent_count
            .cmp(&a.dependent_count)
            .then_with(|| a.path.cmp(&b.path))
    });

    key_files.truncate(max);
    key_files
}

pub(crate) fn format_markdown(map: &ProjectMap) -> String {
    let mut out = String::new();

    // Overview
    out.push_str("# Project Map\n\n");
    out.push_str("## Overview\n\n");

    if let Some(summary) = &map.overview.readme_summary {
        out.push_str(&format!("> {}\n\n", summary));
    }

    out.push_str("### Languages\n\n");
    for (lang, lines) in &map.overview.languages {
        out.push_str(&format!("- **{}**: {} lines\n", lang, lines));
    }
    out.push('\n');

    for manifest in &map.overview.manifests {
        out.push_str("### Manifest\n\n");
        if let Some(name) = &manifest.name {
            out.push_str(&format!("- **Name**: {}\n", name));
        }
        if let Some(desc) = &manifest.description {
            out.push_str(&format!("- **Description**: {}\n", desc));
        }
        if !manifest.dependencies.is_empty() {
            out.push_str("- **Dependencies**:\n");
            for (k, v) in &manifest.dependencies {
                out.push_str(&format!("  - {}: {}\n", k, v));
            }
        }
        if !manifest.dev_dependencies.is_empty() {
            out.push_str("- **Dev Dependencies**:\n");
            for (k, v) in &manifest.dev_dependencies {
                out.push_str(&format!("  - {}: {}\n", k, v));
            }
        }
        if !manifest.scripts.is_empty() {
            out.push_str("- **Scripts**:\n");
            for (k, v) in &manifest.scripts {
                out.push_str(&format!("  - {}: {}\n", k, v));
            }
        }
        out.push('\n');
    }

    if !map.overview.entry_points.is_empty() {
        out.push_str("### Entry Points\n\n");
        for ep in &map.overview.entry_points {
            out.push_str(&format!("- `{}`\n", ep));
        }
        out.push('\n');
    }

    out.push_str(&format!("### Test Files: {}\n\n", map.overview.test_count));

    // Structure
    out.push_str("## Structure\n\n");
    for dir in &map.structure {
        out.push_str(&format!(
            "### {}/ ({} files) — *{}*\n\n",
            dir.path, dir.file_count, dir.role
        ));
        for file in &dir.files {
            out.push_str(&format!("- `{}`\n", file));
        }
        out.push('\n');
    }

    // Key Files
    out.push_str("## Key Files\n\n");
    out.push_str("*Ranked by how many other files depend on them (Aider-style repo map)*\n\n");
    for kf in &map.key_files {
        out.push_str(&format!(
            "### `{}` ({} dependents)\n\n",
            kf.path, kf.dependent_count
        ));
        if let Some(purpose) = &kf.purpose {
            out.push_str(&format!("**Purpose**: {}\n\n", purpose));
        }
        if !kf.symbols.is_empty() {
            out.push_str("**Public Symbols**:\n");
            for sym in &kf.symbols {
                out.push_str(&format!("- {}\n", sym));
            }
            out.push('\n');
        }
        if !kf.dependencies.is_empty() {
            out.push_str("**Depends On**:\n");
            for dep in &kf.dependencies {
                out.push_str(&format!("- `{}`\n", dep));
            }
            out.push('\n');
        }
    }

    // Footer
    out.push_str("---\n\n");
    out.push_str(
        "*This map is generated heuristically using tree-sitter-based symbol/import extraction\n",
    );
    out.push_str("with regex fallback. It may miss dynamic imports, macros, and runtime-only\n");
    out.push_str(
        "dependencies. Purpose lines are extracted from source comments/docstrings where\n",
    );
    out.push_str("available.\n");
    out.push_str("Use this as a starting point; read specific files for details.*\n");

    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{DirectoryInfo, KeyFileInfo, ManifestInfo, Overview, ProjectMap};

    fn file(path: &str, lang: Language) -> FileInfo {
        FileInfo {
            path: path.to_string(),
            language: lang,
            lines: 1,
            symbols: Vec::new(),
            imports: Vec::new(),
            purpose: None,
        }
    }

    #[test]
    fn guess_directory_role_knows_common_names() {
        let empty: Vec<&FileInfo> = vec![];
        assert_eq!(guess_directory_role("tests", &empty), "tests");
        assert_eq!(guess_directory_role("app/routes", &empty), "routes");
        assert_eq!(guess_directory_role("app/models", &empty), "models");
        assert_eq!(guess_directory_role("app/views", &empty), "views");
        assert_eq!(guess_directory_role("app/services", &empty), "services");
        assert_eq!(guess_directory_role("middleware", &empty), "middleware");
        assert_eq!(guess_directory_role("config", &empty), "config");
        assert_eq!(guess_directory_role("db/migrations", &empty), "migrations");
        assert_eq!(guess_directory_role(".", &empty), "root");
    }

    #[test]
    fn guess_directory_role_uses_file_language() {
        let py = [file("src/a.py", Language::Python)];
        let py_refs: Vec<&FileInfo> = py.iter().collect();
        assert_eq!(guess_directory_role("src", &py_refs), "python");
        let ts = [file("src/a.ts", Language::TypeScript)];
        let ts_refs: Vec<&FileInfo> = ts.iter().collect();
        assert_eq!(guess_directory_role("src", &ts_refs), "typescript");
        let rs = [file("src/a.rs", Language::Rust)];
        let rs_refs: Vec<&FileInfo> = rs.iter().collect();
        assert_eq!(guess_directory_role("src", &rs_refs), "rust");
        // Mixed language groups are "other".
        let mixed = [
            file("src/a.py", Language::Python),
            file("src/b.rs", Language::Rust),
        ];
        let mixed_refs: Vec<&FileInfo> = mixed.iter().collect();
        assert_eq!(guess_directory_role("src", &mixed_refs), "other");
    }

    #[test]
    fn rank_key_files_sorts_by_dependents_then_path() {
        let files = vec![
            file("a.rs", Language::Rust),
            file("b.rs", Language::Rust),
            file("c.rs", Language::Rust),
        ];
        let mut graph: DepGraph = HashMap::new();
        graph.insert("a.rs".to_string(), vec!["b.rs".to_string()]);
        graph.insert(
            "b.rs".to_string(),
            vec!["c.rs".to_string(), "c.rs".to_string()],
        );
        graph.insert("c.rs".to_string(), Vec::new());

        let ranked = rank_key_files(&files, &graph, 20);
        // All three appear; c has 2 dependents, b 1, a 0.
        assert!(ranked[0].path == "c.rs" && ranked[0].dependent_count == 2);
        assert!(ranked[1].path == "b.rs" && ranked[1].dependent_count == 1);
        assert!(ranked[2].path == "a.rs" && ranked[2].dependent_count == 0);
        // max is honored.
        let ranked1 = rank_key_files(&files, &graph, 1);
        assert_eq!(ranked1.len(), 1);
        assert_eq!(ranked1[0].path, "c.rs");
    }

    #[test]
    fn markdown_includes_sections_in_order() {
        let empty_deps: BTreeMap<String, String> = BTreeMap::new();
        let map = ProjectMap {
            overview: Overview {
                readme_summary: Some("A summary.".to_string()),
                languages: BTreeMap::from([("Rust".to_string(), 10)]),
                manifests: vec![ManifestInfo {
                    name: Some("demo".to_string()),
                    description: Some("desc".to_string()),
                    dependencies: BTreeMap::from([("serde".to_string(), "1".to_string())]),
                    dev_dependencies: empty_deps,
                    scripts: BTreeMap::from([("test".to_string(), "cargo test".to_string())]),
                }],
                entry_points: vec!["src/main.rs".to_string()],
                test_count: 3,
            },
            structure: vec![DirectoryInfo {
                path: "src".to_string(),
                role: "rust".to_string(),
                file_count: 1,
                files: vec!["src/main.rs".to_string()],
            }],
            key_files: vec![KeyFileInfo {
                path: "src/lib.rs".to_string(),
                purpose: Some("Lib".to_string()),
                symbols: vec!["run".to_string()],
                dependencies: vec!["src/main.rs".to_string()],
                dependent_count: 1,
            }],
        };

        let md = format_markdown(&map);
        let idx_overview = md.find("## Overview").unwrap();
        let idx_structure = md.find("## Structure").unwrap();
        let idx_key_files = md.find("## Key Files").unwrap();
        assert!(idx_overview < idx_structure && idx_structure < idx_key_files);
        assert!(md.contains("### Test Files: 3"));
        assert!(md.contains("### src/ (1 files) — *rust*"));
        assert!(md.contains("- **Dependencies**:"));
        assert!(md.contains("tree-sitter-based symbol/import extraction"));
    }

    #[test]
    fn json_serializes_deterministically() {
        let map = ProjectMap {
            overview: Overview {
                readme_summary: None,
                languages: BTreeMap::from([("Rust".to_string(), 10)]),
                manifests: Vec::new(),
                entry_points: Vec::new(),
                test_count: 0,
            },
            structure: Vec::new(),
            key_files: Vec::new(),
        };
        let a = serde_json::to_string_pretty(&map).unwrap();
        let b = serde_json::to_string_pretty(&map).unwrap();
        assert_eq!(a, b);
        let parsed: ProjectMap = serde_json::from_str(&a).unwrap();
        assert_eq!(parsed.overview.languages.get("Rust"), Some(&10));
    }
}
