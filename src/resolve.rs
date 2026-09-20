//! Resolution of imports to actual file paths.
//!
//! The resolver is *registry first*: `analyze_structure` pre-registers identity,
//! stem, Python dotted-module, and Rust `crate::<module>` keys. This module
//! then layers on language-specific resolution:
//!   - relative specs (`./foo`) expanded per language with `index` fallback,
//!   - Rust `crate::` / `self::` / `super::` paths sanitized and probed as key
//!     chains and (when a crate root is known) as `src/<path>.rs|mod.rs` files.

use std::collections::HashMap;

use crate::Language;

pub(crate) fn resolve_import(
    import: &str,
    current_file: &str,
    lang: Language,
    import_to_file: &HashMap<String, String>,
    crate_root: Option<&str>,
) -> Option<String> {
    let trimmed = import.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('.') {
        return resolve_relative(trimmed, current_file, lang, import_to_file);
    }

    if lang == Language::Rust {
        if let Some(resolved) = resolve_rust(trimmed, current_file, import_to_file, crate_root) {
            return Some(resolved);
        }
        // Fall through so bare single names (`use foo;`) still reach the stem
        // lookup below.
    }

    // Exact registered key: identity, stem, Python dotted module, Rust
    // `crate::...`, or a top-level crate root.
    import_to_file.get(trimmed).cloned()
}

// ---------------------------------------------------------------------------
// Relative imports
// ---------------------------------------------------------------------------

fn relative_extensions(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Python => &[".py", "/__init__.py"],
        Language::Rust => &[".rs"],
        Language::TypeScript => &[".ts", ".tsx"],
        Language::JavaScript => &[".js", ".jsx", ".mjs", ".cjs"],
        Language::Other => &[],
    }
}

/// Collapse "." / ".." components and normalize separators so the result
/// matches the forward-slash keys produced by collect_files.
fn clean_path(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

fn resolve_relative(
    spec: &str,
    current_file: &str,
    lang: Language,
    import_to_file: &HashMap<String, String>,
) -> Option<String> {
    let current_dir = current_file.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let cluster = clean_path(&format!("{}/{spec}", current_dir))
        .trim()
        .to_string();
    if cluster.is_empty() {
        return None;
    }

    let mut candidates: Vec<String> = Vec::new();
    for ext in relative_extensions(lang) {
        candidates.push(format!("{cluster}{ext}"));
    }
    for ext in relative_extensions(lang) {
        candidates.push(format!("{cluster}/index{ext}"));
    }
    for cand in candidates {
        if let Some(target) = import_to_file.get(&cand) {
            return Some(target.clone());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rust: crate:: / self:: / super:: paths, brace lists, and src probing
// ---------------------------------------------------------------------------

/// Detect the crate a Rust file belongs to from its path:
/// `src/x.rs` -> Some(""), `crates/foo/src/x.rs` -> Some("crates/foo"),
/// anything without a `src/` segment -> None.
pub(crate) fn rust_file_crate_root(file: &str) -> Option<String> {
    let parts: Vec<&str> = file.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "src" {
            if i == 0 {
                return Some(String::new());
            }
            return Some(parts[..i].join("/"));
        }
    }
    None
}

/// Module path of a Rust file relative to its crate root, e.g.
/// `crates/foo/src/app/models.rs` -> ["app", "models"]. `main.rs`/`lib.rs`
/// (crate root) yield `[]`; `mod.rs` yields the containing directory only.
/// Returns `None` when the file is outside `<root>/src/`.
pub(crate) fn rust_module_path(file: &str, crate_root: &str) -> Option<Vec<String>> {
    let root_prefix = if crate_root.is_empty() {
        String::new()
    } else {
        format!("{crate_root}/")
    };
    let rest = file.strip_prefix(&root_prefix)?;
    let rest = rest.strip_prefix("src/")?;
    if !rest.ends_with(".rs") {
        return None;
    }
    let mut parts: Vec<String> = rest
        .trim_end_matches(".rs")
        .split('/')
        .map(String::from)
        .collect();
    match parts.last().map(String::as_str) {
        Some("main" | "lib") => {
            parts.pop();
        }
        Some("mod") => {
            // module is the containing directory; parts already correct
            parts.pop();
        }
        _ => {}
    }
    Some(parts)
}

/// Expand `a::{b, c}` and `a::{self, b}` into concrete paths. Single-level
/// brace lists only — everything else is returned unchanged.
fn expand_braces(import: &str) -> Vec<String> {
    let Some(open) = import.find('{') else {
        return vec![import.to_string()];
    };
    let Some(close_rel) = import[open..].find('}') else {
        return vec![import.to_string()];
    };
    let close = open + close_rel;
    let base = import[..open].trim_end_matches(':').trim_end_matches("::");
    let rest = import[close + 1..].trim();
    let inner = &import[open + 1..close];

    let mut out = Vec::new();
    for item in inner.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item == "self" {
            out.push(base.to_string());
            continue;
        }
        // `as` renames a single imported item; the path before it is what maps
        // to a file.
        let item_path = item.split(" as ").next().unwrap_or(item);
        let candidate = if base.is_empty() {
            item_path.to_string()
        } else {
            format!("{base}::{item_path}")
        };
        if !rest.is_empty() {
            out.push(format!("{candidate}::{rest}"));
        } else {
            out.push(candidate);
        }
    }
    if out.is_empty() {
        vec![import.to_string()]
    } else {
        out
    }
}

fn sanitize_rust(path: &str) -> String {
    let path = path.trim();
    let path = path.split_once('{').map(|(p, _)| p).unwrap_or(path).trim();
    let path = path.trim_end_matches(['*', ':']);
    path.split(" as ").next().unwrap_or(path).trim().to_string()
}

fn probe_key_chain(
    segments: &[String],
    import_to_file: &HashMap<String, String>,
) -> Option<String> {
    for i in (1..=segments.len()).rev() {
        let key = format!("crate::{}", segments[..i].join("::"));
        if let Some(target) = import_to_file.get(&key) {
            return Some(target.clone());
        }
    }
    None
}

fn probe_src_chain(
    segments: &[String],
    crate_root: &str,
    import_to_file: &HashMap<String, String>,
) -> Option<String> {
    let root_prefix = if crate_root.is_empty() {
        String::new()
    } else {
        format!("{crate_root}/")
    };
    for i in (1..=segments.len()).rev() {
        let rel = segments[..i].join("/");
        for cand in [
            format!("{root_prefix}src/{rel}.rs"),
            format!("{root_prefix}src/{rel}/mod.rs"),
        ] {
            if let Some(target) = import_to_file.get(&cand) {
                return Some(target.clone());
            }
        }
    }
    None
}

fn probe_chains(
    segments: &[String],
    crate_root: Option<&str>,
    import_to_file: &HashMap<String, String>,
) -> Option<String> {
    if let Some(found) = probe_key_chain(segments, import_to_file) {
        return Some(found);
    }
    if let Some(root) = crate_root {
        if let Some(found) = probe_src_chain(segments, root, import_to_file) {
            return Some(found);
        }
    }
    None
}

fn resolve_rust(
    import: &str,
    current_file: &str,
    import_to_file: &HashMap<String, String>,
    crate_root: Option<&str>,
) -> Option<String> {
    // Split comma/brace lists into concrete paths first: `use a::{b, c};`
    // becomes ["a::b", "a::c"], each resolved independently.
    for spec in expand_braces(import) {
        let path = sanitize_rust(&spec);
        if path.is_empty() {
            continue;
        }

        let resolved = if let Some(rest) = path.strip_prefix("crate::") {
            let segments: Vec<String> = rest.split("::").map(String::from).collect();
            probe_chains(&segments, crate_root, import_to_file)
        } else if let Some(rest) = path.strip_prefix("self::") {
            let mut segments = current_module_path(current_file, crate_root);
            segments.extend(rest.split("::").map(String::from));
            probe_chains(&segments, crate_root, import_to_file)
        } else if let Some(rest) = path.strip_prefix("::") {
            let segments: Vec<String> = rest.split("::").map(String::from).collect();
            probe_chains(&segments, crate_root, import_to_file)
        } else {
            // `super::super::x` — climb the module chain, then probe.
            let mut up = 0;
            let mut remaining = path.as_str();
            while remaining.starts_with("super::") {
                up += 1;
                remaining = &remaining["super::".len()..];
            }
            if up > 0 {
                let mut segments = current_module_path(current_file, crate_root);
                let climb = up.min(segments.len());
                segments.truncate(segments.len().saturating_sub(climb));
                segments.extend(remaining.split("::").map(String::from));
                probe_chains(&segments, crate_root, import_to_file)
            } else {
                // Bare `foo` / `foo::bar`: crate-relative in Rust 2018+. A bare
                // single path that registers as a stem is handled by the caller.
                let segments: Vec<String> = path.split("::").map(String::from).collect();
                probe_chains(&segments, crate_root, import_to_file)
            }
        };

        if resolved.is_some() {
            return resolved;
        }
    }
    None
}

fn current_module_path(current_file: &str, crate_root: Option<&str>) -> Vec<String> {
    if let Some(root) = crate_root {
        if let Some(mp) = rust_module_path(current_file, root) {
            return mp;
        }
    }
    // Without a crate root we cannot know the module chain; fall back to the
    // crate root itself so `self::x` behaves like `crate::x`.
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn rust_crate_import_resolves_via_registry() {
        let map = registry(&[
            ("src/lib.rs", "src/lib.rs"),
            ("crate", "src/lib.rs"),
            ("src/app/models.rs", "src/app/models.rs"),
            ("crate::app::models", "src/app/models.rs"),
            ("src/app/routes.rs", "src/app/routes.rs"),
            ("crate::app::routes", "src/app/routes.rs"),
        ]);

        assert_eq!(
            resolve_import(
                "crate::app::models",
                "src/main.rs",
                Language::Rust,
                &map,
                Some("")
            ),
            Some("src/app/models.rs".to_string())
        );
        // A short prefix that is itself a registered module also resolves.
        assert_eq!(
            resolve_import("crate", "src/main.rs", Language::Rust, &map, Some("")),
            Some("src/lib.rs".to_string())
        );
        // Unknown modules do not resolve.
        assert_eq!(
            resolve_import(
                "crate::app::missing",
                "src/main.rs",
                Language::Rust,
                &map,
                Some("")
            ),
            None
        );
    }

    #[test]
    fn rust_crate_import_probes_src_path() {
        // No `crate::...` registry keys; only the file identity exists. The
        // probe must locate `src/services/db.rs` from the path directly.
        let map = registry(&[("src/services/db.rs", "src/services/db.rs")]);
        assert_eq!(
            resolve_import(
                "crate::services::db",
                "src/main.rs",
                Language::Rust,
                &map,
                Some("")
            ),
            Some("src/services/db.rs".to_string())
        );
        // mod.rs form is probed too.
        let map2 = registry(&[("src/services/db/mod.rs", "src/services/db/mod.rs")]);
        assert_eq!(
            resolve_import(
                "crate::services::db::Client",
                "src/main.rs",
                Language::Rust,
                &map2,
                Some("")
            ),
            Some("src/services/db/mod.rs".to_string())
        );
    }

    #[test]
    fn rust_self_and_super_resolve() {
        let map = registry(&[
            ("crate::app", "src/app/mod.rs"),
            ("crate::app::router", "src/app/router.rs"),
            ("crate::app::models", "src/app/models.rs"),
        ]);
        // `self::router` from src/app/mod.rs: module path is ["app"].
        assert_eq!(
            resolve_import(
                "self::router",
                "src/app/mod.rs",
                Language::Rust,
                &map,
                Some("")
            ),
            Some("src/app/router.rs".to_string())
        );
        // `super::models` from src/app/router.rs: module path ["app","router"],
        // one super climbs to ["app"].
        assert_eq!(
            resolve_import(
                "super::models",
                "src/app/router.rs",
                Language::Rust,
                &map,
                Some("")
            ),
            Some("src/app/models.rs".to_string())
        );
        // `super::super::x` from a nested file climbs two levels.
        let map2 = registry(&[("crate::app::handlers::util", "src/app/handlers/util.rs")]);
        assert_eq!(
            resolve_import(
                "super::super::handlers::util",
                "src/app/handlers/check.rs",
                Language::Rust,
                &map2,
                Some("")
            ),
            Some("src/app/handlers/util.rs".to_string())
        );
    }

    #[test]
    fn rust_brace_imports_expand_and_resolve() {
        let map = registry(&[
            ("crate::models", "src/models.rs"),
            ("crate::views::index", "src/views/index.rs"),
        ]);
        // `use crate::models::{User, Post};` — neither sub-name is its own
        // module, but the chain probe finds `crate::models`.
        assert_eq!(
            resolve_import(
                "crate::models::{User, Post}",
                "src/main.rs",
                Language::Rust,
                &map,
                Some("")
            ),
            Some("src/models.rs".to_string())
        );
        // A brace item that names a real module resolves too.
        assert_eq!(
            resolve_import(
                "crate::views::{index, self}",
                "src/main.rs",
                Language::Rust,
                &map,
                Some("")
            ),
            Some("src/views/index.rs".to_string())
        );
    }

    #[test]
    fn python_dotted_import_resolves() {
        let map = registry(&[
            ("app", "app/__init__.py"),
            ("app/__init__.py", "app/__init__.py"),
            ("app.models", "app/models.py"),
            ("app/models.py", "app/models.py"),
        ]);
        assert_eq!(
            resolve_import("app.models", "app/main.py", Language::Python, &map, None),
            Some("app/models.py".to_string())
        );
    }

    #[test]
    fn relative_python_resolves_with_index_fallback() {
        let map = registry(&[
            ("src/__init__.py", "src/__init__.py"),
            ("src/helpers.py", "src/helpers.py"),
            ("src/views/__init__.py", "src/views/__init__.py"),
        ]);
        // `from . import helpers` inside src/...
        assert_eq!(
            resolve_import("./helpers", "src/app.py", Language::Python, &map, None),
            Some("src/helpers.py".to_string())
        );
        // `from . import views` resolves to the package __init__.
        assert_eq!(
            resolve_import("./views", "src/app.py", Language::Python, &map, None),
            Some("src/views/__init__.py".to_string())
        );
    }

    #[test]
    fn relative_typescript_resolves_with_index_fallback() {
        let map = registry(&[("components/ui/index.ts", "components/ui/index.ts")]);
        assert_eq!(
            resolve_import(
                "./ui",
                "components/layout.ts",
                Language::TypeScript,
                &map,
                None
            ),
            Some("components/ui/index.ts".to_string())
        );
    }

    #[test]
    fn bare_stem_and_identity_resolve() {
        let map = registry(&[
            ("util", "src/util.rs"),
            ("src/util.rs", "src/util.rs"),
            ("helpers", "src/helpers.py"),
        ]);
        assert_eq!(
            resolve_import("util", "src/main.rs", Language::Rust, &map, Some("")),
            Some("src/util.rs".to_string())
        );
        assert_eq!(
            resolve_import("helpers", "src/main.rs", Language::Python, &map, None),
            Some("src/helpers.py".to_string())
        );
        // Empty and unknown imports return None.
        assert_eq!(
            resolve_import("", "src/main.rs", Language::Python, &map, None),
            None
        );
        assert_eq!(
            resolve_import("nope", "src/main.rs", Language::Python, &map, None),
            None
        );
    }

    #[test]
    fn crate_root_detection() {
        assert_eq!(rust_file_crate_root("src/main.rs"), Some("".to_string()));
        assert_eq!(
            rust_file_crate_root("crates/foo/src/lib.rs"),
            Some("crates/foo".to_string())
        );
        assert_eq!(rust_file_crate_root("tests/it.rs"), None);
        assert!(rust_file_crate_root("crates/foo/src/app/mod.rs")
            .is_some_and(|root| root == "crates/foo"));
    }

    #[test]
    fn module_path_maps_files() {
        assert_eq!(
            rust_module_path("src/lib.rs", ""),
            Some(Vec::<String>::new())
        );
        assert_eq!(
            rust_module_path("src/app/models.rs", ""),
            Some(vec!["app".to_string(), "models".to_string()])
        );
        // mod.rs contributes only its directory.
        assert_eq!(
            rust_module_path("src/app/mod.rs", ""),
            Some(vec!["app".to_string()])
        );
        assert_eq!(
            rust_module_path("crates/foo/src/db.rs", "crates/foo"),
            Some(vec!["db".to_string()])
        );
    }
}
