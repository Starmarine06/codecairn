//! Symbol, import, and purpose extraction from source files.
//!
//! Primary path is tree-sitter (syntax-aware, handles multi-line imports and
//! exotic formatting). If parsing fails or yields nothing, we fall back to the
//! line-based regex extractors that powered v0.1. Purpose lines stay regex
//! based in both paths — comments/docstrings are not part of the syntax tree
//! we care about.

use regex::Regex;
use tree_sitter::{Node, Parser};

use crate::Language;

pub(crate) fn extract_symbols_imports_purpose(
    content: &str,
    lang: Language,
) -> (Vec<String>, Vec<String>, Option<String>) {
    if lang == Language::Other {
        return (Vec::new(), Vec::new(), None);
    }

    if let Some((symbols, imports)) = tree_sitter_extract(content, lang) {
        if !symbols.is_empty() || !imports.is_empty() {
            return (symbols, imports, purpose_of(content, lang));
        }
    }

    match lang {
        Language::Python => extract_python(content),
        Language::TypeScript => extract_typescript(content),
        Language::Rust => extract_rust(content),
        Language::JavaScript => extract_javascript(content),
        Language::Other => (Vec::new(), Vec::new(), None),
    }
}

// ---------------------------------------------------------------------------
// Tree-sitter primary extraction
// ---------------------------------------------------------------------------

fn tree_sitter_extract(content: &str, lang: Language) -> Option<(Vec<String>, Vec<String>)> {
    let mut parser = Parser::new();
    let grammar: tree_sitter::Language = match lang {
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Other => return None,
    };
    parser.set_language(&grammar).ok()?;

    let tree = parser.parse(content, None)?;
    let mut symbols = Vec::new();
    let mut imports = Vec::new();

    let mut stack: Vec<Node<'_>> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        // Container nodes that describe imports/exports are handled fully
        // below and skipped by the generic child traversal so nothing is
        // double-counted. Everything else keeps being walked into.
        let handled = match lang {
            Language::Python => collect_python(node, content, &mut symbols, &mut imports),
            Language::TypeScript => collect_typescript(node, content, &mut symbols, &mut imports),
            Language::Rust => collect_rust(node, content, &mut symbols, &mut imports),
            Language::JavaScript => collect_javascript(node, content, &mut symbols, &mut imports),
            Language::Other => false,
        };
        if handled {
            continue;
        }
        for child in named_children(node) {
            stack.push(child);
        }
    }

    Some((symbols, imports))
}

/// Iterate a node's named children without forcing every call site to thread a
/// `TreeCursor`. Mirrors the ownership pattern of `Node::named_children(&mut
/// TreeCursor)` in tree-sitter 0.27+ while keeping the iterator self-contained.
fn named_children<'tree>(node: Node<'tree>) -> impl Iterator<Item = Node<'tree>> + 'tree {
    let mut cursor = node.walk();
    cursor.reset(node);
    cursor.goto_first_child();
    (0..node.named_child_count()).map(move |_| {
        while !cursor.node().is_named() {
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        let result = cursor.node();
        cursor.goto_next_sibling();
        result
    })
}

fn node_text(node: Node<'_>, src: &str) -> Option<String> {
    node.utf8_text(src.as_bytes()).ok().map(|s| s.to_string())
}

fn filename_path<'a>(node: Node<'_>, src: &'a str) -> Option<&'a str> {
    node.utf8_text(src.as_bytes()).ok()
}

/// Reconstruct a relative import spec for `from .foo import x` /
/// `from .. import y` / `from . import helpers` so the resolver can find the
/// defining module, mirroring the v0.1 regex behavior.
fn relative_import_spec(module: &Node<'_>, stmt: &Node<'_>, src: &str) -> Option<String> {
    let module_text = node_text(*module, src)?;
    let depth = module_text.chars().take_while(|c| *c == '.').count();
    for child in named_children(*module) {
        if child.kind() == "dotted_name" {
            if let Some(name) = node_text(child, src) {
                if depth > 1 {
                    // `from ..pkg import x` — represent as ./pkg (single level,
                    // matching the regex fallback).
                    return Some(format!("./{}", name.replace('.', "/")));
                }
                return Some(format!("./{}", name));
            }
        }
    }
    // `from . import helpers, util` — use the first imported name.
    for child in named_children(*stmt) {
        let name = match child.kind() {
            "dotted_name" => node_text(child, src),
            "aliased_import" => child
                .child_by_field_name("name")
                .and_then(|n| node_text(n, src)),
            _ => None,
        };
        if let Some(name) = name {
            return Some(format!("./{}", name.split('.').next().unwrap_or("")));
        }
    }
    None
}

fn collect_python(
    node: Node<'_>,
    src: &str,
    symbols: &mut Vec<String>,
    imports: &mut Vec<String>,
) -> bool {
    match node.kind() {
        "function_definition" | "class_definition" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(n, src))
            {
                symbols.push(name);
            }
            false // keep walking (nested defs/imports)
        }
        "import_statement" => {
            for child in named_children(node) {
                let name = match child.kind() {
                    "dotted_name" => node_text(child, src),
                    "aliased_import" => child
                        .child_by_field_name("name")
                        .and_then(|n| node_text(n, src)),
                    _ => None,
                };
                if let Some(name) = name {
                    imports.push(name);
                }
            }
            true
        }
        "import_from_statement" => {
            if let Some(module) = node.child_by_field_name("module_name") {
                if module.kind() == "relative_import" {
                    if let Some(spec) = relative_import_spec(&module, &node, src) {
                        imports.push(spec);
                    }
                } else if let Some(name) = node_text(module, src) {
                    imports.push(name);
                }
            }
            true
        }
        _ => false,
    }
}

fn declared_name(node: Node<'_>, src: &str) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        if let Some(text) = node_text(name, src) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    // Fallback: walk for the first identifier/type identifier (e.g. inside a
    // `lexical_declaration` for `export const PORT = 3000`).
    let mut stack: Vec<Node<'_>> = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() != node.kind() && matches!(n.kind(), "identifier" | "type_identifier") {
            if let Some(text) = node_text(n, src) {
                return Some(text);
            }
        }
        for child in named_children(n) {
            stack.push(child);
        }
    }
    None
}

fn collect_typescript(
    node: Node<'_>,
    src: &str,
    symbols: &mut Vec<String>,
    imports: &mut Vec<String>,
) -> bool {
    match node.kind() {
        "import_statement" => {
            if let Some(source) = node
                .child_by_field_name("source")
                .and_then(|n| node_text(n, src))
            {
                imports.push(trim_quotes(&source));
            }
            true
        }
        "export_statement" => {
            if let Some(decl) = node.child_by_field_name("declaration") {
                if let Some(name) = declared_name(decl, src) {
                    symbols.push(name);
                }
            } else if let Some(source) = node
                .child_by_field_name("source")
                .and_then(|n| node_text(n, src))
            {
                imports.push(trim_quotes(&source));
            }
            true
        }
        _ => false,
    }
}

fn collect_rust(
    node: Node<'_>,
    src: &str,
    symbols: &mut Vec<String>,
    imports: &mut Vec<String>,
) -> bool {
    match node.kind() {
        // Only pub items, matching the v0.1 regex (`pub fn|struct|enum|...`).
        "function_item" | "struct_item" | "enum_item" | "trait_item" | "type_item"
        | "const_item" | "static_item" | "union_item" | "mod_item" => {
            if named_children(node).any(|c| c.kind() == "visibility_modifier") {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, src))
                {
                    symbols.push(name);
                }
            }
            false // keep walking (nested functions, tests, etc.)
        }
        "use_declaration" => {
            if let Some(arg) = node.child_by_field_name("argument") {
                if let Some(text) = node_text(arg, src) {
                    imports.push(text);
                }
            }
            true
        }
        _ => false,
    }
}

fn trim_quotes(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

fn collect_javascript(
    node: Node<'_>,
    src: &str,
    symbols: &mut Vec<String>,
    imports: &mut Vec<String>,
) -> bool {
    match node.kind() {
        "import_statement" => {
            if let Some(source) = node
                .child_by_field_name("source")
                .and_then(|n| node_text(n, src))
            {
                imports.push(trim_quotes(&source));
            }
            true
        }
        "export_statement" => {
            if let Some(decl) = node.child_by_field_name("declaration") {
                if let Some(name) = declared_name(decl, src) {
                    symbols.push(name);
                }
            } else if let Some(source) = node
                .child_by_field_name("source")
                .and_then(|n| node_text(n, src))
            {
                imports.push(trim_quotes(&source));
            }
            true
        }
        "call_expression" => {
            // require('...')
            let func = node.child_by_field_name("function");
            let is_require = func
                .filter(|f| f.kind() == "identifier")
                .and_then(|f| filename_path(f, src))
                .map(|t| t == "require")
                .unwrap_or(false);
            if is_require {
                if let Some(args) = node.child_by_field_name("arguments") {
                    for child in named_children(args) {
                        if child.kind() == "string" {
                            if let Some(text) = node_text(child, src) {
                                imports.push(trim_quotes(&text));
                                break;
                            }
                        }
                    }
                }
                return true;
            }
            false
        }
        "assignment_expression" => {
            // module.exports = Foo / exports.foo = Foo
            let left = node.child_by_field_name("left");
            let left_text = left.and_then(|n| node_text(n, src));
            if let Some(lt) = left_text {
                if lt == "module.exports" || lt.starts_with("exports.") {
                    if let Some(right) = node.child_by_field_name("right") {
                        if right.kind() == "identifier" {
                            if let Some(name) = node_text(right, src) {
                                symbols.push(name);
                                return true;
                            }
                        }
                    }
                }
            }
            false
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Purpose extraction (regex, shared by both extraction paths)
// ---------------------------------------------------------------------------

fn purpose_of(content: &str, lang: Language) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        match lang {
            Language::Python => {
                if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
                    return Some(trimmed.trim_matches('"').trim_matches('\'').to_string());
                }
                if trimmed.starts_with('#') && !trimmed.starts_with("#!") {
                    return Some(trimmed.trim_start_matches('#').trim().to_string());
                }
            }
            Language::TypeScript | Language::JavaScript => {
                if trimmed.starts_with("/**") {
                    return Some(
                        trimmed
                            .trim_start_matches("/**")
                            .trim_end_matches("*/")
                            .trim()
                            .to_string(),
                    );
                }
                if trimmed.starts_with("//") {
                    return Some(trimmed.trim_start_matches("//").trim().to_string());
                }
            }
            Language::Rust => {
                if trimmed.starts_with("///")
                    || trimmed.starts_with("//!")
                    || trimmed.starts_with("/*!")
                {
                    return Some(
                        trimmed
                            .trim_start_matches("///")
                            .trim_start_matches("//!")
                            .trim_start_matches("/*!")
                            .trim_end_matches("*/")
                            .trim()
                            .to_string(),
                    );
                }
            }
            Language::Other => {}
        }
        if !trimmed.is_empty() {
            return None;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Regex fallback (v0.1 extractors, kept so the tree-sitter switch is safe)
// ---------------------------------------------------------------------------

pub(crate) fn extract_python(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

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

    let symbol_re = Regex::new(r"^\s*(?:async\s+)?(?:def|class)\s+(\w+)").unwrap();
    for line in &lines {
        if let Some(caps) = symbol_re.captures(line) {
            symbols.push(caps[1].to_string());
        }
    }

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

pub(crate) fn extract_typescript(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

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

    let import_re =
        Regex::new(r#"^\s*import\s+(?:[\w\s{},*]+\s+from\s+)?['"]([^'"]+)['"]"#).unwrap();
    for line in &lines {
        if let Some(caps) = import_re.captures(line) {
            imports.push(caps[1].to_string());
        }
    }

    (symbols, imports, purpose)
}

pub(crate) fn extract_rust(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

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

    let symbol_re =
        Regex::new(r"^\s*pub\s+(?:fn|struct|enum|trait|mod|const|static|type)\s+(\w+)").unwrap();
    for line in &lines {
        if let Some(caps) = symbol_re.captures(line) {
            symbols.push(caps[1].to_string());
        }
    }

    let import_re = Regex::new(r"^\s*use\s+([\w:]+(?:\s*\{[^}]*\})?)").unwrap();
    for line in &lines {
        if let Some(caps) = import_re.captures(line) {
            imports.push(caps[1].to_string());
        }
    }

    (symbols, imports, purpose)
}

pub(crate) fn extract_javascript(content: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut purpose = None;

    let lines: Vec<&str> = content.lines().collect();

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

    let symbol_re =
        Regex::new(r"(?:module\.exports|exports\.)\s*=\s*(\w+)|^\s*(?:class|function)\s+(\w+)")
            .unwrap();
    for line in &lines {
        if let Some(caps) = symbol_re.captures(line) {
            let symbol = if let Some(m) = caps.get(1) {
                Some(m.as_str().to_string())
            } else {
                caps.get(2).map(|m| m.as_str().to_string())
            };
            if let Some(symbol) = symbol {
                if !symbols.contains(&symbol) {
                    symbols.push(symbol);
                }
            }
        }
    }

    let import_re = Regex::new(r#"require\(['"]([^'"]+)['"]\)"#).unwrap();
    for line in &lines {
        for caps in import_re.captures_iter(line) {
            imports.push(caps[1].to_string());
        }
    }

    (symbols, imports, purpose)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_tree_sitter_finds_defs_imports_and_purpose() {
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
        let (symbols, imports, purpose) = extract_symbols_imports_purpose(src, Language::Python);
        assert!(symbols.contains(&"RequestHelper".to_string()));
        assert!(symbols.contains(&"parse_body".to_string()));
        assert!(imports.contains(&"flask".to_string()));
        assert!(imports.contains(&"json".to_string()));
        assert_eq!(purpose.as_deref(), Some("HTTP helpers for the app."));
    }

    #[test]
    fn python_relative_imports_become_dot_specs() {
        let src = "from . import helpers\nfrom ..services import db\nfrom .models import User\n";
        let (_, imports, _) = extract_symbols_imports_purpose(src, Language::Python);
        assert!(imports.contains(&"./helpers".to_string()));
        assert!(imports.contains(&"./services".to_string()));
        assert!(imports.contains(&"./models".to_string()));
    }

    #[test]
    fn typescript_finds_exports_imports_and_purpose() {
        let src = "// Route definitions for v1\n\
                   import express from 'express'\n\
                   import { Router } from './router'\n\
                   \n\
                   export class App {\n\
                       constructor() {}\n\
                   }\n\
                   export function start(): void {}\n\
                   export const PORT = 3000;\n";
        let (symbols, imports, purpose) =
            extract_symbols_imports_purpose(src, Language::TypeScript);
        assert!(symbols.contains(&"App".to_string()));
        assert!(symbols.contains(&"start".to_string()));
        assert!(symbols.contains(&"PORT".to_string()));
        assert!(imports.contains(&"express".to_string()));
        assert!(imports.contains(&"./router".to_string()));
        assert_eq!(purpose.as_deref(), Some("Route definitions for v1"));
    }

    #[test]
    fn rust_finds_pub_items_and_uses() {
        let src = "/// Query helpers shared across modules.\n\
                   use std::collections::HashMap;\n\
                   use crate::models::{User, Post};\n\
                   \n\
                   pub fn find_by_id<T>(id: T) {}\n\
                   pub struct Query {}\n\
                   pub enum SortOrder { Asc, Desc }\n";
        let (symbols, imports, purpose) = extract_symbols_imports_purpose(src, Language::Rust);
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
    fn rust_ignores_private_items() {
        let src = "fn hidden() {}\nstruct Private {}\nuse std::fmt;\n";
        let (symbols, imports, _) = extract_symbols_imports_purpose(src, Language::Rust);
        assert!(symbols.is_empty());
        assert_eq!(imports, vec!["std::fmt".to_string()]);
    }

    #[test]
    fn javascript_finds_require_and_exports() {
        let src = "// Logger for the app\n\
                   const path = require('path');\n\
                   const express = require('express');\n\
                   module.exports = init;\n\
                   function init() {}\n";
        let (symbols, imports, purpose) =
            extract_symbols_imports_purpose(src, Language::JavaScript);
        assert!(symbols.contains(&"init".to_string()));
        assert!(imports.contains(&"path".to_string()));
        assert!(imports.contains(&"express".to_string()));
        assert_eq!(purpose.as_deref(), Some("Logger for the app"));
    }

    #[test]
    fn unsupported_language_yields_nothing() {
        let (symbols, imports, purpose) =
            extract_symbols_imports_purpose("garbage", Language::Other);
        assert!(symbols.is_empty());
        assert!(imports.is_empty());
        assert_eq!(purpose, None);
    }

    #[test]
    fn regex_fallbacks_still_work() {
        let py = "#!/usr/bin/env python\n\ndef utility():\n    pass\n";
        let (symbols, imports, purpose) = extract_python(py);
        assert_eq!(symbols, vec!["utility"]);
        assert!(imports.is_empty());
        assert_eq!(purpose, None);

        let js = "module.exports = sum;\nfunction sum(a, b) {}\n";
        let (symbols, _, _) = extract_javascript(js);
        assert_eq!(symbols, vec!["sum"]);
    }
}
