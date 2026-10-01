//! Hermes fork: askama only tracks the template files it resolved, so a new file in
//! `templates-hermes/` (a shadow of an upstream template, docs/hermes-theme.md) wouldn't trigger a
//! rebuild on its own. Watching both directories (cargo scans them recursively) and the askama
//! config makes adding, changing or removing a shadow recompile the crate.

fn main() {
    println!("cargo:rerun-if-changed=askama.toml");
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=templates-hermes");
}
