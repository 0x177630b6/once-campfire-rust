//! Hermes fork: template shadowing (`askama.toml` searches `templates-hermes/` before `templates/`,
//! docs/hermes-theme.md). Guards what askama can't:
//! - every file in `templates-hermes/` is listed in its `SHADOWED.md`, and every row names an
//!   upstream template that exists (a shadow of nothing would be a silent new template);
//! - no template refers to another by a path askama resolves relative to the including file first
//!   (a bare name), which would skip `templates-hermes/`;
//! - no `include_str!` in `src/` reads an upstream template that is shadowed (the fragment-cache
//!   digests and the service worker embed files by path, so they'd keep upstream's copy).
//!
//! `crates/views/script/check-shadowed` runs the same checks after an upstream merge, plus whether
//! each shadowed template changed upstream since it was copied.

use std::path::{Component, Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn upstream_dir() -> PathBuf {
    crate_dir().join("templates")
}

fn shadow_dir() -> PathBuf {
    crate_dir().join("templates-hermes")
}

/// Files under `root`, as `/`-separated paths relative to it, sorted.
fn files(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                out.push(relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The shadowed templates: everything in `templates-hermes/` but its two documents.
fn shadowed_files() -> Vec<String> {
    files(&shadow_dir()).into_iter().filter(|path| path != "README.md" && path != "SHADOWED.md").collect()
}

#[derive(Debug, PartialEq)]
struct Row {
    shadowed: String,
    upstream: String,
    blob: String,
}

/// `SHADOWED.md`'s table rows (the ones whose first cell is a `code` path).
fn rows(markdown: &str) -> Vec<Row> {
    markdown
        .lines()
        .filter(|line| line.trim_start().starts_with("| `"))
        .map(|line| {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(|cell| cell.trim().trim_matches('`')).collect();
            assert!(cells.len() >= 4, "SHADOWED.md: a row needs 4 cells: {line}");
            Row { shadowed: cells[0].to_string(), upstream: cells[1].to_string(), blob: cells[2].to_string() }
        })
        .collect()
}

/// The template paths in `{% include "…" %}`, `{% extends "…" %}` and `{% import "…" as … %}`.
fn template_refs(source: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("{%") {
        rest = &rest[start + 2..];
        let end = rest.find("%}").unwrap_or(rest.len());
        let tag = rest[..end].trim_start_matches(['-', '+', '~']).trim_start();
        for keyword in ["include", "extends", "import"] {
            if let Some(after) = tag.strip_prefix(keyword).filter(|after| after.starts_with(char::is_whitespace)) {
                let after = after.trim_start();
                if let Some(quoted) = after.strip_prefix('"') {
                    refs.push(quoted.split('"').next().unwrap_or_default().to_string());
                }
            }
        }
        rest = &rest[end..];
    }
    refs
}

/// Why askama might resolve `reference`, written in the template at `file` (relative to its
/// directory), without looking in `templates-hermes/` first; `None` when it's rooted.
fn bypasses_the_shadow(file: &str, reference: &str, roots: &[PathBuf]) -> Option<String> {
    if !reference.contains('/') {
        return Some(format!("{file}: \"{reference}\" is a bare name"));
    }
    let sibling = Path::new(file).with_file_name(reference);
    roots
        .iter()
        .any(|root| root.join(&sibling).exists())
        .then(|| format!("{file}: \"{reference}\" resolves next to the including file ({})", sibling.display()))
}

/// `include_str!("…")` paths in `source`.
fn included_strs(source: &str) -> Vec<String> {
    source.split("include_str!(\"").skip(1).filter_map(|rest| rest.split('"').next()).map(str::to_string).collect()
}

/// `base.join(relative)` with `..` and `.` folded (the files may not exist).
fn normalize(base: &Path, relative: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in base.join(relative).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Every `include_str!` of an upstream template in `src/`, as (source file, template path).
fn embedded_templates() -> Vec<(String, String)> {
    let src = crate_dir().join("src");
    let mut out = Vec::new();
    for file in files(&src).into_iter().filter(|file| file.ends_with(".rs")) {
        let path = src.join(&file);
        let source = std::fs::read_to_string(&path).unwrap();
        for included in included_strs(&source) {
            let target = normalize(path.parent().unwrap(), &included);
            if let Ok(template) = target.strip_prefix(upstream_dir()) {
                out.push((file.clone(), template.to_string_lossy().replace('\\', "/")));
            }
        }
    }
    out
}

#[test]
fn every_shadowed_template_is_listed_and_exists_upstream() {
    let listed = rows(&std::fs::read_to_string(shadow_dir().join("SHADOWED.md")).expect("templates-hermes/SHADOWED.md"));
    let shadowed = shadowed_files();
    for file in &shadowed {
        assert!(listed.iter().any(|row| &row.shadowed == file), "templates-hermes/{file} is not listed in SHADOWED.md");
    }
    for row in &listed {
        assert!(shadowed.contains(&row.shadowed), "SHADOWED.md lists {}, which isn't in templates-hermes/", row.shadowed);
        assert_eq!(row.upstream, row.shadowed, "a shadow replaces the upstream template at the same path");
        assert!(upstream_dir().join(&row.upstream).is_file(), "{} shadows templates/{}, which doesn't exist", row.shadowed, row.upstream);
        assert!(row.blob.len() == 40 && row.blob.bytes().all(|b| b.is_ascii_hexdigit()), "{}: blob {:?}", row.shadowed, row.blob);
    }
}

#[test]
fn templates_refer_to_each_other_by_rooted_paths() {
    let roots = [upstream_dir(), shadow_dir()];
    let mut problems = Vec::new();
    for root in &roots {
        for file in files(root).into_iter().filter(|file| !file.ends_with(".md")) {
            let Ok(source) = std::fs::read_to_string(root.join(&file)) else { continue };
            for reference in template_refs(&source) {
                problems.extend(bypasses_the_shadow(&file, &reference, &roots));
            }
        }
    }
    assert!(problems.is_empty(), "these would skip templates-hermes/:\n{}", problems.join("\n"));
}

#[test]
fn no_embedded_template_is_shadowed() {
    let embedded = embedded_templates();
    // The scanner still finds the ones the README names.
    for (file, template) in [
        ("messages.rs", "messages/_message.html"),
        ("messages.rs", "messages/boosts/_boost.html"),
        ("users.rs", "users/sidebars/rooms/_direct.html"),
        ("pwa.rs", "pwa/service_worker.js"),
    ] {
        assert!(embedded.iter().any(|e| e.0 == file && e.1 == template), "{file} embeds {template}: {embedded:?}");
    }
    for (file, template) in embedded {
        assert!(
            !shadow_dir().join(&template).exists(),
            "templates-hermes/{template} is shadowed, but src/{file} still embeds templates/{template} with include_str!: \
             point it at the shadow (or the fragment cache keeps upstream's markup)"
        );
    }
}

#[test]
fn the_checks_catch_what_they_should() {
    let source = r#"{% extends "layouts/application.html" %}{%- include "rooms/show/_nav.html" %}{% include "_bare.html" %}
        {%+ import "macros/x.html" as x %}{% if a %}{% include  "a/b.html" -%}{% endif %}{% block include %}{% endblock %}"#;
    assert_eq!(template_refs(source), ["layouts/application.html", "rooms/show/_nav.html", "_bare.html", "macros/x.html", "a/b.html"]);
    let roots = [upstream_dir()];
    assert!(bypasses_the_shadow("rooms/show.html", "_bare.html", &roots).is_some());
    assert!(bypasses_the_shadow("rooms/show.html", "rooms/show/_nav.html", &roots).is_none());
    // `show/_nav.html` from `rooms/show.html` exists as `rooms/show/_nav.html`: relative.
    assert!(bypasses_the_shadow("rooms/show.html", "show/_nav.html", &roots).is_some());
    assert_eq!(
        rows("| Shadowed | x |\n|---|\n| `a/b.html` | `a/b.html` | `0123` | why |\n"),
        [Row { shadowed: "a/b.html".into(), upstream: "a/b.html".into(), blob: "0123".into() }]
    );
    assert_eq!(included_strs(r#"include_str!("../templates/a.html"), include_str!("x")"#), ["../templates/a.html", "x"]);
    assert_eq!(normalize(Path::new("/c/src/messages"), "../../templates/a.html"), PathBuf::from("/c/templates/a.html"));
}
