//! codecairn: a model-free project map for AI assistants.
//!
//! Reads a codebase and prints a Markdown (or JSON) map — overview, directory
//! structure with guessed roles, and a ranked list of the files everything else
//! depends on. No model involved anywhere.

mod analyze;
mod extract;
mod format;
mod resolve;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::analyze::{
    analyze_structure, collect_files, collect_manifests, count_languages, count_tests,
    extract_readme_summary, find_entry_points,
};
use crate::format::{format_markdown, rank_key_files};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// file path -> paths it imports from (local, resolved)
pub(crate) type DepGraph = std::collections::HashMap<String, Vec<String>>;

#[derive(Parser, Debug)]
#[command(
    name = "codecairn",
    version = VERSION,
    about = "Model-free project map for AI assistants"
)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Markdown,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Language {
    Python,
    TypeScript,
    Rust,
    JavaScript,
    Other,
}

impl Language {
    pub(crate) fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "py" => Language::Python,
            "ts" | "tsx" => Language::TypeScript,
            "rs" => Language::Rust,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            _ => Language::Other,
        }
    }

    pub(crate) fn name(self) -> &'static str {
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
pub(crate) struct FileInfo {
    pub(crate) path: String,
    pub(crate) language: Language,
    pub(crate) lines: usize,
    pub(crate) symbols: Vec<String>,
    pub(crate) imports: Vec<String>,
    pub(crate) purpose: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ManifestInfo {
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) dependencies: BTreeMap<String, String>,
    pub(crate) dev_dependencies: BTreeMap<String, String>,
    pub(crate) scripts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DirectoryInfo {
    pub(crate) path: String,
    pub(crate) role: String,
    pub(crate) file_count: usize,
    pub(crate) files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct KeyFileInfo {
    pub(crate) path: String,
    pub(crate) purpose: Option<String>,
    pub(crate) symbols: Vec<String>,
    pub(crate) dependencies: Vec<String>,
    pub(crate) dependent_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProjectMap {
    pub(crate) overview: Overview,
    pub(crate) structure: Vec<DirectoryInfo>,
    pub(crate) key_files: Vec<KeyFileInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Overview {
    pub(crate) readme_summary: Option<String>,
    pub(crate) languages: BTreeMap<String, usize>,
    pub(crate) manifests: Vec<ManifestInfo>,
    pub(crate) entry_points: Vec<String>,
    pub(crate) test_count: usize,
}

fn main() {
    let args = Args::parse();
    if let Err(e) = run(args) {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let project_root = fs::canonicalize(&args.path).context("invalid path")?;

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
        fs::write(&output_path, output)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
    } else {
        write_stdout(&output)?;
    }

    Ok(())
}

fn write_stdout(output: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let result = stdout
        .write_all(output.as_bytes())
        .and_then(|_| stdout.flush());
    if let Err(e) = result {
        // `codecairn | head` and friends are normal usage, not an error.
        if e.kind() != io::ErrorKind::BrokenPipe {
            return Err(e).context("writing output");
        }
    }
    Ok(())
}
