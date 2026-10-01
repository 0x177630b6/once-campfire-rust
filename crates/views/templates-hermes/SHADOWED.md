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
| `users/profiles/show.html` | `users/profiles/show.html` | `d3990332d54a660a9573a06cf1538cb8f1cce0d5` | The theme's Light / Dark / System switch (`crate::hermes::color_scheme_switch`), before the memberships. One added seam call, nothing else changed; it renders nothing until the app installs the page assets with the theme on, so the goldens keep upstream's bytes |
| `accounts/_help_contact.html` | `accounts/_help_contact.html` | `7b1dcdf86c8dd738cf6c3f5825668cf9af5a244e` | Branding (Meshduty): the footer's "Campfire&trade; version" is `crate::hermes::product_trademark()` ("Meshduty"). Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `accounts/_invite.html` | `accounts/_invite.html` | `c01855f5340d7fdf447d6c778b3c4a0c47b0161f` | Branding (Meshduty): the invite link's share title and text go through `crate::hermes::rebrand`. Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `accounts/bots/index.html` | `accounts/bots/index.html` | `7fd7bff394d54fe57dd3e15052642f97805b8aaa` | Branding (Meshduty): "post updates directly to Campfire" names `crate::hermes::product_name()`. Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `accounts/edit.html` | `accounts/edit.html` | `a56f3646ea7ed34210c2479a561df7d820d4f3d0` | Branding (Meshduty): the footer's "Campfire&trade; version" is `crate::hermes::product_trademark()`. Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `layouts/application.html` | `layouts/application.html` | `951a07f00c2f721fe6c5af60fe588e60bf318a48` | Branding (Meshduty): the `theme-color` metas are `crate::hermes::theme_color_tags()` (the theme's surface, light and dark, with the theme on) the favicon and the `apple-touch-icon` are `crate::hermes::favicon_url(ctx)` and `apple_touch_icon_url(ctx)` (a 32 px and a full-bleed default while no logo is uploaded) and the desktop corner logo is `crate::hermes::app_logo(ctx)` (the product's mark linking home). The fork's two in-place seams of the upstream file (head_tags, workspace_overlay) are kept as they are. Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `pwa/_install_instructions.html` | `pwa/_install_instructions.html` | `9431e3efdd200b6fe1225d9b306cb63e10a9f010` | Branding (Meshduty): the 4 "Campfire" are `crate::hermes::product_name()` (the Edge icon's alt text through `rebrand`). Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `pwa/_system_settings.html` | `pwa/_system_settings.html` | `1dbc84e3ae968d6a638a1d2f693a7c0257eb6ae9` | Branding (Meshduty): the 6 "Campfire" are `crate::hermes::product_name()`. Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `pwa/manifest.json` | `pwa/manifest.json` | `5de90dbe8e96b3827acec0adf590f99d015f43de` | Branding (Meshduty): name fallback, `short_name`, the default maskable icon, description, `theme_color` / `background_color`, the shortcuts' descriptions, and no upstream screenshots once branded (`Manifest`'s methods in `src/pwa.rs`). `start_url` and `scope` unchanged, no `id` added (the app's identity stays `/`). Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `rooms/show/_invitation.html` | `rooms/show/_invitation.html` | `2c82fa3318a115888eb0efaf220fd19d8d66823d` | Branding (Meshduty): "Welcome to Campfire" names `crate::hermes::product_name()`. Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
| `sessions/incompatible_browser.html` | `sessions/incompatible_browser.html` | `d727b557c98ad42b9c8097b4717a5b575cd48481` | Branding (Meshduty): "Campfire requires a modern web browser" names `crate::hermes::product_name()`. Renders upstream's bytes until the app installs the branding, so the goldens need no allowlist |
