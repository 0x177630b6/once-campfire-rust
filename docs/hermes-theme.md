# Hermes fork: the theme layer

The plumbing for the product's own look (the "theme layer", option B of the UI redesign handbook in
the Hermes repo, `docs/ui-redesign/`). It changes nothing visible by itself: `theme.css` is empty,
and the fork's stylesheets render as they did. What it gives the designer:

- one stylesheet that always wins, `crates/assets/overrides/hermes/theme.css`;
- the fork's own stylesheets in `<head>`, before the theme, so equal-specificity ties go to the theme;
- template shadowing for the few screens CSS can't reach (`crates/views/templates-hermes/`).

## Load order

The layout's head (`crates/views/templates/layouts/application.html`) renders, in this order:

| # | What | Where it comes from |
|---|---|---|
| 1 | Upstream's stylesheets (`stylesheet_link_tag :all`) | `ctx.stylesheet_tags`, the reference's exact list (`crates/assets/tests/reference.rs`) |
| 2 | The account's Custom styles (an inline `<style>`) | `custom_styles_tag`, from the database |
| 3 | `hermes/hermes.css` (voice features) | the head seam, `campfire_views::hermes::head_tags` |
| 4 | `hermes/workspace.css`, only on a signed-in person's pages while the workspace is on | the head seam, through `WorkspaceHooks::stylesheets` |
| 5 | `hermes/theme.css`, unless `CAMPFIRE_THEME=off` | the head seam |

Every link from the seam has `data-turbo-track="reload"`, like upstream's stylesheets: an installed
app left open reloads at its next visit after a deploy changed one of them. The list is the same on
every page of a signed-in person, which matters: Turbo reloads the whole page when a visit's tracked
elements differ from the current page's. (Signing in or out changes the list by `workspace.css`,
which costs one full load at that moment.)

Owner decision (1 Oct 2026): the theme is the product's main theme, so it loads **after** Custom
styles and always wins. Custom styles are for prototyping only; empty them on every deployment
when the theme ships. Note that the fork's stylesheets (3, 4) also come after Custom styles, as
they did when they were linked from the body: while prototyping in Custom styles, write fork
tokens as `:root:root { … }` to win the tie.

## The switch: `CAMPFIRE_THEME`

`on` (the default) or `off` (`false`, `0`, `no`); any other value stops the app at boot. `off`
leaves out `theme.css` only: the fork's stylesheets stay, since the voice features and the
workspace need them. Use it to tell a theme bug from an app bug, or to compare with upstream's look.

How the views' tests keep upstream's bytes: the seam renders nothing until the app installs its
page assets at boot (`campfire_views::hermes::install_page_assets`, called from
`controllers::workspace::start`, workspace on or off). The views' goldens and DOM-parity tests never
install them, the same way they never install the workspace's hooks, so they compare upstream's
markup unchanged. `crates/views/tests/hermes_head.rs` checks the installed order, the switch, and
that removing the assets gives upstream's bytes back. The app's own request tests boot the app, so
their pages carry the links (theme on).

## What moved (no visual change intended)

- `hermes.css` was linked from the voice page's head block and from inside the composer; it is now
  linked from the head of every page. Its rules match only the voice features' markup, with one
  exception made explicit: the touch-screen button size `.composer .btn` now reads
  `.composer:has(:where(#composer)) .btn` (same specificity), so the search page's look-alike bar
  keeps upstream's size, as before. An audio attachment shown outside a room (search results, for
  instance) now gets the same player styles as in the room; before, it had none there.
- `workspace.css` was linked by the tab bar partial inside `<body>`; it is now in the head, for the
  same people (signed in, not bots) while the workspace is on. The workspace's scripts stay with the
  tab bar.

## Template shadowing

`crates/views/askama.toml` searches `templates-hermes/` before `templates/`, so a file at
`crates/views/templates-hermes/<path>` replaces upstream's `templates/<path>` everywhere askama
looks it up by path: `#[template(path = …)]`, `{% include %}`, `{% extends %}`. The directory holds
only its README and `SHADOWED.md` for now: no template is shadowed.

| Piece | What it does |
|---|---|
| `crates/views/build.rs` | `cargo:rerun-if-changed` on `templates`, `templates-hermes` and `askama.toml`: askama only tracks the files it resolved, so without it a new shadow wouldn't trigger a rebuild |
| `templates-hermes/SHADOWED.md` | One row per shadowed file: path, upstream source (the same path), `git hash-object` of the upstream file when copied, reason |
| `tests/shadowed_templates.rs` (`cargo test -p campfire_views`) | Every file in `templates-hermes/` is listed and its upstream template exists; no template refers to another by a path askama resolves relative to the including file (a bare name: it would skip `templates-hermes/`); no `include_str!` in `src/` embeds a shadowed template |
| `crates/views/script/check-shadowed` | The same checks, plus each shadowed template whose upstream file changed since the copy (with the diff). Exit 1 = broken, 2 = changes to review. In the "After each upstream merge" checklist (docs/hermes-workspace.md) |

Embedded templates: the fragment-cache digests (`src/messages.rs`, `message_digest` and
`boost_digest`: `messages/_message`, `_actions`, `_presentation`, `_unrenderable`,
`boosts/_boosts`, `boosts/_boost`; `src/users.rs`, `direct_room_digest`:
`users/sidebars/rooms/_direct`) and the service worker (`src/pwa.rs`: `pwa/service_worker.js`)
read upstream's files with `include_str!`, which ignores `askama.toml`. Shadowing one of them
without pointing its `include_str!` at the shadow would keep the old digest (cached fragments with
the old markup survive) or serve upstream's service worker. Rather than resolve those paths at
build time, the test fails as soon as such a file is shadowed and its `include_str!` still reads
`templates/`: change the path to `../templates-hermes/…` in the same commit (a marked
`Hermes fork:` seam, listed in docs/hermes-workspace.md).

Goldens: the views' goldens render whatever askama resolves, so a shadowed template changes every
golden that renders it (all pages, for `layouts/application.html`). There is no golden allowlist
yet; add one with the first shadow (a list of golden names skipped by `parity_a.rs`'s
`assert_parity` and `messages_support`'s comparison, kept next to `SHADOWED.md`), and keep the
markup that code reads (handbook chapter 03) checked by hand or by fork-owned tests.
