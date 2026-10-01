# Shadowed templates

Every file under `crates/views/templates-hermes/` (other than this list and the README) shadows the
upstream template at the same path in `crates/views/templates/`. One row per file:

- **Shadowed**: the path under `templates-hermes/`;
- **Upstream source**: the path under `templates/` it replaces (the same path: that's how shadowing works);
- **Upstream blob at copy**: `git hash-object crates/views/templates/<path>` when the copy was made
  or last re-ported, 40 hex digits; `crates/views/script/check-shadowed` reports when upstream's
  file no longer has it;
- **Reason**: why CSS couldn't do it.

Checked by `crates/views/tests/shadowed_templates.rs` (every file listed, every row's upstream file
exists) and `crates/views/script/check-shadowed` (also upstream changes since the copy).

| Shadowed | Upstream source | Upstream blob at copy | Reason |
|---|---|---|---|
| `users/profiles/show.html` | `users/profiles/show.html` | `d3990332d54a660a9573a06cf1538cb8f1cce0d5` | The theme's Light / Dark / System switch (`crate::hermes::theme_switch`), before the memberships. One added seam call, nothing else changed; it renders nothing until the app installs the page assets with the theme on, so the goldens keep upstream's bytes |
