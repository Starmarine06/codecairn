use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use ignore::WalkBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// file path -> paths it imports from (local, resolved)
type DepGraph = HashMap<String, Vec<String>>;

#[derive(Parser, Debug)]
#[command(name = "codecairn", version = VERSION, about = "Model-free project map for AI assistants")]
struct Args {
    /// Path to project root (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Output format
    #[arg(short, long, value_enum, default_value = "markdown")]
    format: OutputFormat,

    /// Maximum number of key files to show
    #[arg(long, default_value = "20")]
    max_key_files: usize,

    /// Include hidden files/directories
    #[arg(long)]
    include_hidden: bool,

    /// Output to file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Markdown,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileInfo {
    path: String,
    language: Language,
    lines: usize,
    symbols: Vec<String>,
    imports: Vec<String>,
    purpose: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Language {
    Python,
    TypeScript,
    Rust,
    JavaScript,
    Other,
}

impl Language {
    fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "py" => Language::Python,
            "ts" | "tsx" => Language::TypeScript,
            "rs" => Language::Rust,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            _ => Language::Other,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Language::Python => "Python",
            Language::TypeScript => "TypeScript",
            Language::Rust => "Rust",
            Language::JavaScript => "JavaScript",
            Language::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestInfo {
    name: Option<String>,
    description: Option<String>,
    dependencies: HashMap<String, String>,
    dev_dependencies: HashMap<String, String>,
    scripts: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DirectoryInfo {
    path: String,
    role: String,
    file_count: usize,
    files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyFileInfo {
    path: String,
    purpose: Option<String>,
    symbols: Vec<String>,
    dependencies: Vec<String>,
    dependent_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectMap {
    overview: Overview,
    structure: Vec<DirectoryInfo>,
    key_files: Vec<KeyFileInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Overview {
    readme_summary: Option<String>,
    languages: HashMap<String, usize>,
    manifests: Vec<ManifestInfo>,
    entry_points: Vec<String>,
    test_count: usize,
}

fn main() {
    let args = Args::parse();

    if let Err(e) = run(args) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let project_root = fs::canonicalize(&args.path).context("Invalid path")?;

    let file_infos = collect_files(&project_root, args.include_hidden)?;
    let manifest_infos = collect_manifests(&project_root)?;
    let entry_points = find_entry_points(&project_root, &file_infos)?;
    let test_count = count_tests(&file_infos);
    let readme_summary = extract_readme_summary(&project_root);

    let (dir_infos, dep_graph) = analyze_structure(&project_root, &file_infos)?;
    let key_files = rank_key_files(&file_infos, &dep_graph, args.max_key_files);

    let overview = Overview {
        readme_summary,
        languages: count_languages(&file_infos),
        manifests: manifest_infos,
        entry_points,
        test_count,
    };

    let project_map = ProjectMap {
        overview,
        structure: dir_infos,
        key_files,
    };

    let output = match args.format {
        OutputFormat::Markdown => format_markdown(&project_map),
        OutputFormat::Json => serde_json::to_string_pretty(&project_map)?,
    };

    if let Some(output_path) = args.output {
        fs::write(output_path, output)?;
    } else {
        let result = (|| -> io::Result<()> {
            let mut stdout = io::stdout().lock();
            stdout.write_all(output.as_bytes())?;
            stdout.flush()
        })();
        if let Err(e) = result {
            if e.kind() != io::ErrorKind::BrokenPipe {
                return Err(e).context("writing output");
            }
        }
    }

    Ok(())
}

fn collect_files(root: &Path, include_hidden: bool) -> Result<Vec<FileInfo>> {
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

fn should_skip(path: &str) -> bool {
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

fn extract_symbols_imports_purpose(
    content: &str,
    lang: Language,
) -> (Vec<String>, Vec<String>, Option<String>) {
    match lang {
        Language::Python => extract_python(content),
        Language::TypeScript => extract_typescript(content),
        Language::Rust => extract_rust(content),
        Language::JavaScript => extract_javascript(content),
        Language::Other => (Vec::new(), Vec::new(), None),
    }
}

fn extract_python(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

    // Purpose: first docstring or comment block
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
            purpose = Some(trimmed.trim_matches('"').trim_matches('\'').to_string());
            break;
        }
        if trimmed.starts_with("#") && !trimmed.starts_with("#!") {
            purpose = Some(trimmed.trim_start_matches('#').trim().to_string());
            break;
        }
        if !trimmed.is_empty() {
            break;
        }
    }

    // Symbols: class, def, async def
    let symbol_re = Regex::new(r"^\s*(?:async\s+)?(?:def|class)\s+(\w+)").unwrap();
    for line in &lines {
        if let Some(caps) = symbol_re.captures(line) {
            symbols.push(caps[1].to_string());
        }
    }

    // Imports: from X import a, b / import X
    // For relative imports (from .foo import x, from ..pkg import y, from . import z)
    // record a local-relative dependency spec like "./foo" / "./pkg" / "./z" so the
    // resolver can find the defining module.
    let from_re = Regex::new(r"^\s*from\s+([\w.]+)\s+import\s+([\w\s,]+)").unwrap();
    let import_re = Regex::new(r"^\s*import\s+([\w.]+)").unwrap();
    for line in &lines {
        if let Some(caps) = from_re.captures(line) {
            let base = caps[1].to_string();
            if base.starts_with('.') {
                let relative = base.trim_start_matches('.');
                let spec = if relative.is_empty() {
                    caps[2]
                        .split(',')
                        .next()
                        .map(|s| s.trim().split('.').next().unwrap_or("").to_string())
                        .map(|name| format!("./{}", name))
                        .unwrap_or_default()
                } else {
                    format!("./{}", relative.replace('.', "/"))
                };
                if !spec.is_empty() {
                    imports.push(spec);
                }
            } else {
                imports.push(base);
            }
        } else if let Some(caps) = import_re.captures(line) {
            imports.push(caps[1].to_string());
        }
    }

    (symbols, imports, purpose)
}

fn extract_typescript(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

    // Purpose: first JSDoc or comment
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("/**") {
            purpose = Some(
                trimmed
                    .trim_start_matches("/**")
                    .trim_end_matches("*/")
                    .trim()
                    .to_string(),
            );
            break;
        }
        if trimmed.starts_with("//") {
            purpose = Some(trimmed.trim_start_matches("//").trim().to_string());
            break;
        }
        if !trimmed.is_empty() {
            break;
        }
    }

    // Symbols: export (class|function|const|let|var|interface|type) name
    let symbol_re =
        Regex::new(r"^\s*export\s+(?:class|function|const|let|var|interface|type)\s+(\w+)")
            .unwrap();
    let default_export_re =
        Regex::new(r"^\s*export\s+default\s+(?:class|function)?\s*(\w+)").unwrap();
    for line in &lines {
        if let Some(caps) = symbol_re.captures(line) {
            symbols.push(caps[1].to_string());
        } else if let Some(caps) = default_export_re.captures(line) {
            symbols.push(caps[1].to_string());
        }
    }

    // Imports: import ... from '...'
    let import_re =
        Regex::new(r#"^\s*import\s+(?:[\w\s{},*]+\s+from\s+)?['"]([^'"]+)['"]"#).unwrap();
    for line in &lines {
        if let Some(caps) = import_re.captures(line) {
            imports.push(caps[1].to_string());
        }
    }

    (symbols, imports, purpose)
}

fn extract_rust(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

    // Purpose: first doc comment /// or //! or /*!
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("///") || trimmed.starts_with("//!") || trimmed.starts_with("/*!") {
            purpose = Some(
                trimmed
                    .trim_start_matches("///")
                    .trim_start_matches("//!")
                    .trim_start_matches("/*!")
                    .trim_end_matches("*/")
                    .trim()
                    .to_string(),
            );
            break;
        }
        if !trimmed.is_empty() {
            break;
        }
    }

    // Symbols: pub (fn|struct|enum|trait|mod|const|static|type) name
    let symbol_re =
        Regex::new(r"^\s*pub\s+(?:fn|struct|enum|trait|mod|const|static|type)\s+(\w+)").unwrap();
    for line in &lines {
        if let Some(caps) = symbol_re.captures(line) {
            symbols.push(caps[1].to_string());
        }
    }

    // Imports: use crate::... or use std::... or extern crate
    // Captures the module path and any brace group, e.g. `crate::models::{User, Post}`.
    let import_re = Regex::new(r"^\s*use\s+([\w:]+(?:\s*\{[^}]*\})?)").unwrap();
    for line in &lines {
        if let Some(caps) = import_re.captures(line) {
            imports.push(caps[1].to_string());
        }
    }

    (symbols, imports, purpose)
}

fn extract_javascript(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

    // Purpose: first JSDoc or comment
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("/**") {
            purpose = Some(
                trimmed
                    .trim_start_matches("/**")
                    .trim_end_matches("*/")
                    .trim()
                    .to_string(),
            );
            break;
        }
        if trimmed.starts_with("//") {
            purpose = Some(trimmed.trim_start_matches("//").trim().to_string());
            break;
        }
        if !trimmed.is_empty() {
            break;
        }
    }

    // Symbols: module.exports, exports., class, function
    let symbol_re =
        Regex::new(r"(?:module\.exports|exports\.)\s*=\s*(\w+)|^\s*(?:class|function)\s+(\w+)")
            .unwrap();
    for line in &lines {
        if let Some(caps) = symbol_re.captures(line) {
            if let Some(m) = caps.get(1) {
                symbols.push(m.as_str().to_string());
            } else if let Some(m) = caps.get(2) {
                symbols.push(m.as_str().to_string());
            }
        }
    }

    // Imports: require('...')
    let import_re = Regex::new(r#"require\(['"]([^'"]+)['"]\)"#).unwrap();
    for line in &lines {
        for caps in import_re.captures_iter(line) {
            imports.push(caps[1].to_string());
        }
    }

    (symbols, imports, purpose)
}

fn collect_manifests(root: &Path) -> Result<Vec<ManifestInfo>> {
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
                    scripts: HashMap::new(),
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
                    dev_dependencies: HashMap::new(),
                    scripts: HashMap::new(),
                });
            }
        }
    }

    Ok(manifests)
}

fn find_entry_points(root: &Path, files: &[FileInfo]) -> Result<Vec<String>> {
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

fn count_tests(files: &[FileInfo]) -> usize {
    files
        .iter()
        .filter(|f| {
            f.path.contains("test") || f.path.contains("spec") || f.path.contains("__tests__")
        })
        .count()
}

fn extract_readme_summary(root: &Path) -> Option<String> {
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

fn count_languages(files: &[FileInfo]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for file in files {
        *counts.entry(file.language.name().to_string()).or_insert(0) += file.lines;
    }
    counts
}

fn analyze_structure(_root: &Path, files: &[FileInfo]) -> Result<(Vec<DirectoryInfo>, DepGraph)> {
    let mut dir_map: HashMap<String, Vec<&FileInfo>> = HashMap::new();
    let mut dep_graph: DepGraph = HashMap::new();

    // Group files by directory
    for file in files {
        let dir = Path::new(&file.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        dir_map.entry(dir).or_default().push(file);
    }

    // Build import -> file mapping for resolution
    let mut import_to_file: HashMap<String, String> = HashMap::new();
    for file in files {
        import_to_file.insert(file.path.clone(), file.path.clone());
        // Also map by stem name
        if let Some(stem) = Path::new(&file.path).file_stem().and_then(|s| s.to_str()) {
            import_to_file.insert(stem.to_string(), file.path.clone());
        }
    }

    // Resolve imports to local files
    for file in files {
        let mut resolved_deps = Vec::new();
        for import in &file.imports {
            // Try to resolve relative imports
            if let Some(resolved) = resolve_import(import, &file.path, &import_to_file) {
                if resolved != file.path {
                    resolved_deps.push(resolved);
                }
            }
        }
        dep_graph.insert(file.path.clone(), resolved_deps);
    }

    // Create directory infos with guessed roles
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

fn resolve_import(
    import: &str,
    current_file: &str,
    import_to_file: &HashMap<String, String>,
) -> Option<String> {
    // Collapse "." / ".." components and normalize separators to "/" so the result
    // matches the forward-slash keys produced by collect_files.
    fn clean_path(p: &Path) -> String {
        let mut out: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                Component::Normal(s) => out.push(s.to_string_lossy().to_string()),
                _ => {}
            }
        }
        out.join("/")
    }

    // Handle relative imports: "./foo/bar", "../pkg/mod", "./helpers"
    if import.starts_with('.') {
        let current_dir = Path::new(current_file).parent()?;
        let base = clean_path(&current_dir.join(import));

        let mut candidates = vec![base.clone()];
        for ext in [".py", ".rs", ".ts", ".tsx", ".js", ".jsx"] {
            candidates.push(format!("{}{}", base, ext));
        }

        for c in candidates {
            if import_to_file.contains_key(&c) {
                return Some(c);
            }
        }
    }

    // Try direct mapping (also matches module stems like "helpers" -> src/helpers.py)
    import_to_file.get(import).cloned()
}

fn guess_directory_role(dir: &str, files: &[&FileInfo]) -> String {
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

fn rank_key_files(files: &[FileInfo], dep_graph: &DepGraph, max: usize) -> Vec<KeyFileInfo> {
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

fn format_markdown(map: &ProjectMap) -> String {
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
        "*This map is generated heuristically using regex-based symbol/import extraction.\n",
    );
    out.push_str("It may miss dynamic imports, macros, and runtime-only dependencies.\n");
    out.push_str("Purpose lines are extracted from source comments/docstrings where available.\n");
    out.push_str("Use this as a starting point; read specific files for details.*\n");

    out
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

    #[test]
    fn python_extraction_finds_symbols_imports_purpose() {
        let src = "\"\"\"HTTP helpers for the app.\"\"\"\n\
                   from flask import request\n\
                   import json\n\
                   \n\
                   class RequestHelper:\n\
                       def get_json(self):\n\
                           return json.loads(request.data)\n\
                   \n\
                   async def parse_body(body):\n\
                       return body\n";
        let (symbols, imports, purpose) = extract_python(src);
        assert!(symbols.contains(&"RequestHelper".to_string()));
        assert!(symbols.contains(&"parse_body".to_string()));
        assert!(imports.contains(&"flask".to_string()));
        assert!(imports.contains(&"json".to_string()));
        assert_eq!(purpose.as_deref(), Some("HTTP helpers for the app."));
    }

    #[test]
    fn typescript_extraction_finds_exports_imports() {
        let src = "// Route definitions for v1\n\
                   import express from 'express'\n\
                   import { Router } from './router'\n\
                   \n\
                   export class App {\n\
                       constructor() {}\n\
                   }\n\
                   export function start(): void {}\n\
                   export const PORT = 3000;\n";
        let (symbols, imports, purpose) = extract_typescript(src);
        assert!(symbols.contains(&"App".to_string()));
        assert!(symbols.contains(&"start".to_string()));
        assert!(symbols.contains(&"PORT".to_string()));
        assert!(imports.contains(&"express".to_string()));
        assert!(imports.contains(&"./router".to_string()));
        assert_eq!(purpose.as_deref(), Some("Route definitions for v1"));
    }

    #[test]
    fn rust_extraction_finds_pub_items_and_uses() {
        let src = "/// Query helpers shared across modules.\n\
                   use std::collections::HashMap;\n\
                   use crate::models::{User, Post};\n\
                   \n\
                   pub fn find_by_id<T>(id: T) {}\n\
                   pub struct Query {}\n\
                   pub enum SortOrder { Asc, Desc }\n";
        let (symbols, imports, purpose) = extract_rust(src);
        assert!(symbols.contains(&"find_by_id".to_string()));
        assert!(symbols.contains(&"Query".to_string()));
        assert!(symbols.contains(&"SortOrder".to_string()));
        assert!(imports.contains(&"std::collections::HashMap".to_string()));
        assert!(imports.contains(&"crate::models::{User, Post}".to_string()));
        assert_eq!(
            purpose.as_deref(),
            Some("Query helpers shared across modules.")
        );
    }

    #[test]
    fn should_skip_ignores_build_dirs_and_hidden() {
        assert!(should_skip("node_modules/pkg/index.js"));
        assert!(should_skip("target/release/codecairn"));
        assert!(should_skip("src/.secret.py"));
        assert!(should_skip("Cargo.lock"));
        assert!(!should_skip("src/main.rs"));
        assert!(!should_skip("tests/test_app.py"));
    }

    #[test]
    fn resolve_import_maps_stem_and_relative() {
        let mut map = HashMap::new();
        map.insert("src/main.py".to_string(), "src/main.py".to_string());
        map.insert("main".to_string(), "src/main.py".to_string());
        map.insert(
            "src/helpers/util.py".to_string(),
            "src/helpers/util.py".to_string(),
        );

        assert_eq!(
            resolve_import("main", "anything.py", &map),
            Some("src/main.py".to_string())
        );

        // Relative import from src/app.py -> util resolves to src/helpers/util.py? No, that's wrong
        // Relative import resolution is directory-relative: from src/app.py, `helpers.util` stays dotted.
        // Explicitly: a relative spec starting with "." joins the current file's directory.
        map.insert(
            "src/helpers/util.py".to_string(),
            "src/helpers/util.py".to_string(),
        );
        assert_ne!(resolve_import("./helpers/util", "src/app.py", &map), None);
    }

    #[test]
    fn guess_directory_role_knows_common_names() {
        let empty: Vec<&FileInfo> = vec![];
        assert_eq!(guess_directory_role("tests", &empty), "tests");
        assert_eq!(guess_directory_role("app/routes", &empty), "routes");
        assert_eq!(guess_directory_role("app/models", &empty), "models");
        assert_eq!(guess_directory_role("app/views", &empty), "views");
        assert_eq!(guess_directory_role("app/services", &empty), "services");
        assert_eq!(guess_directory_role("config", &empty), "config");
        assert_eq!(guess_directory_role(".", &empty), "root");
    }

    #[test]
    fn golden_fixture_generates_expected_map() {
        let dir = std::env::temp_dir().join(format!("codecairn-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        write_fixture(
            &dir,
            "README.md",
            "# demo\n\nA small demo project used for tests.\n",
        );
        write_fixture(&dir, "package.json", "{\"name\":\"demo\",\"description\":\"demo\",\"dependencies\":{\"express\":\"^4\"},\"scripts\":{\"test\":\"jest\"}}");
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
        let (_dirs, dep_graph) = analyze_structure(&dir, &files).unwrap();
        let key_files = rank_key_files(&files, &dep_graph, 20);

        assert!(files.iter().any(|f| f.path.contains("main.py")));
        assert!(!files.iter().any(|f| f.path.contains("node_modules")));
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name.as_deref(), Some("demo"));
        assert!(test_count >= 1);

        // helpers.py should be imported by main.py, so it should have >= 1 dependent
        let helper = key_files.iter().find(|k| k.path.contains("helpers.py"));
        let (helper_deps, helper_syms) = match helper {
            Some(k) => (k.dependent_count, &k.symbols),
            None => (0, &Vec::new()),
        };
        assert!(helper_deps >= 1);
        assert!(helper_syms.contains(&"Helper".to_string()));

        // main.py depends on helpers
        let main = files.iter().find(|f| f.path.contains("main.py")).unwrap();
        let deps = dep_graph.get(&main.path).cloned().unwrap_or_default();
        assert!(deps.iter().any(|d| d.contains("helpers")));

        let _ = fs::remove_dir_all(&dir);
    }
}
