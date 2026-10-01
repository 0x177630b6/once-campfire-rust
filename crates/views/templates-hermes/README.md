# Shadowed templates (Hermes fork)

A file here at the same path as one in `../templates/` replaces it: `askama.toml` searches
`templates-hermes` first, so `#[template(path = …)]`, `{% include %}` and `{% extends %}` with a
rooted path all pick this copy. Use it only where CSS (`crates/assets/overrides/hermes/theme.css`)
can't do the job. The list is in [`SHADOWED.md`](SHADOWED.md).

Before adding a file:

1. Copy the upstream file to the same path here, and record it in [`SHADOWED.md`](SHADOWED.md)
   with `git hash-object crates/views/templates/<path>` (the upstream content you started from)
   and the reason. `tests/shadowed_templates.rs` fails for an unlisted file.
2. Keep every id, class and data attribute that code reads (handbook chapter 03 in the Hermes
   repo): the goldens no longer guard the markup you change.
3. If the file is one whose content upstream embeds with `include_str!` (the message and sidebar
   fragment-cache digests in `src/messages.rs` and `src/users.rs`, the service worker in
   `src/pwa.rs`), point that `include_str!` at the copy here in the same commit, or cached
   fragments keep the old markup (`tests/shadowed_templates.rs` fails until you do).
4. Use rooted paths in `{% include %}` / `{% extends %}` (`"rooms/show/_composer.html"`): askama
   resolves a path relative to the including file first, so a bare name skips this directory.
5. Upstream goldens that render the shadowed file will differ: add them to the golden allowlist
   (to be created with the first shadow, see docs/hermes-theme.md).

After each upstream merge run `crates/views/script/check-shadowed` (docs/hermes-workspace.md,
"After each upstream merge").
