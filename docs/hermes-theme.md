# Hermes fork: the theme layer

The plumbing for the product's own look (the "theme layer", option B of the UI redesign handbook in
the Hermes repo, `docs/ui-redesign/`). It changes nothing visible by itself: `theme.css` is empty,
and the fork's stylesheets render as they did. What it gives the designer:

- one stylesheet that always wins, `crates/assets/overrides/hermes/theme.css`;
- the fork's own stylesheets in `<head>`, before the theme, so equal-specificity ties go to the theme;
- one small set of semantic tokens (`--ui-*`, `hermes/tokens.css`) that the Workspace and the voice
  features read, so a theme recolours Campfire, the Workspace and voice together;
- template shadowing for the few screens CSS can't reach (`crates/views/templates-hermes/`).

## Load order

The layout's head (`crates/views/templates/layouts/application.html`) renders, in this order:

| # | What | Where it comes from |
|---|---|---|
| 1 | Upstream's stylesheets (`stylesheet_link_tag :all`) | `ctx.stylesheet_tags`, the reference's exact list (`crates/assets/tests/reference.rs`) |
| 2 | The account's Custom styles (an inline `<style>`) | `custom_styles_tag`, from the database |
| 3 | `hermes/tokens.css` (the fork's `--ui-*` tokens, below) | the head seam, `campfire_views::hermes::head_tags`, with any of the fork's stylesheets |
| 4 | `hermes/hermes.css` (voice features) | the head seam |
| 5 | `hermes/workspace.css`, only on a signed-in person's pages while the workspace is on | the head seam, through `WorkspaceHooks::stylesheets` |
| 6 | `hermes/theme.css`, unless `CAMPFIRE_THEME=off` | the head seam |

Every link from the seam has `data-turbo-track="reload"`, like upstream's stylesheets: an installed
app left open reloads at its next visit after a deploy changed one of them. The list is the same on
every page of a signed-in person, which matters: Turbo reloads the whole page when a visit's tracked
elements differ from the current page's. (Signing in or out changes the list by `workspace.css`,
which costs one full load at that moment.)

Owner decision (1 Oct 2026): the theme is the product's main theme, so it loads **after** Custom
styles and always wins. Custom styles are for prototyping only; empty them on every deployment
when the theme ships. Note that the fork's stylesheets (3 to 5) also come after Custom styles, as
they did when they were linked from the body: while prototyping in Custom styles, write fork
tokens as `:root:root { … }` to win the tie.

## The switch: `CAMPFIRE_THEME`

`on` (the default) or `off` (`false`, `0`, `no`); any other value stops the app at boot. `off`
leaves out `theme.css` only: the tokens and the fork's stylesheets stay, since the voice features and the
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
- `workspace.css` and `hermes.css` read the `--ui-*` tokens instead of upstream's `--color-*` and
  literal colours, radii, shadows and gutters (same computed values, see "Tokens"). The two
  `.ws-filters` rules (board and Hermes tab) are one rule with the values the second one already
  imposed on both.

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

## Tokens

Three layers, each built on the one before:

1. **Upstream's tokens** (`reference/app/assets/stylesheets/colors.css`, `utilities.css`): 12
   `--lch-*` triplets (`L C H`, no `oklch()` around them) feeding 13 `--color-*`, and the spacing
   `--inline-space` (1ch) and `--block-space` (1rem) with their `-half` / `-double`. Redefining the
   `--lch-*` triplets recolours Campfire **and** the fork, since the fork's colour tokens point at
   them by default.
2. **The fork's semantic tokens**, `--ui-*` in `crates/assets/overrides/hermes/tokens.css`: what
   the Workspace (`workspace.css`) and the voice features (`hermes.css`) read for every colour,
   radius, shadow and page gutter. Redefine one in `theme.css` only where the fork should differ
   from upstream, or for what upstream has no token for (AI, radius, shadows).
3. **The Workspace's domain tokens**, `--ws-*` at the top of `workspace.css`: the severity scale,
   Fizzy's column tones and a few aliases. Most aliases point at `--ui-*`; the severity and column
   colours are still the Workspace's own (they mirror Fizzy's palette).

Every default reproduces the rendering from before the tokens existed, in light and dark:
checked declaration by declaration (each token expanded to its default, light and dark, against
the previous files), and by comparing the computed styles of ~3,800 elements in headless Chrome
(light and dark, 400 and 1,400 px wide) with the old and new stylesheets: no difference. Dark mode
still follows `prefers-color-scheme` (the manual light/dark switch is a later task: restate the
dark values under a `[data-theme="dark"]` root attribute then).

A `--ui-*` token defined from another (`--ui-text-muted` from `--ui-text`) follows a theme that
redefines the base one at `:root`. Redefine tokens on `:root` (or `:root:root` from Custom
styles); a token redefined on an inner element would not change the `:root` tokens built from it.

**For existing installs:** the `--ui-*` tokens are resolved once, on `:root`. Before this change
`workspace.css` and `hermes.css` read `--color-*` on each element, so Custom styles that redefine
`--color-*` on an inner element (`#sidebar { --color-bg: … }`, `.message { --color-text: … }`)
also recoloured the Workspace and voice elements inside it. They no longer do: those elements now
take the `:root` value through `--ui-*`. Redefinitions on `:root` still reach everything. Check the
account's Custom styles on each deployment (Account → Custom styles) for such rules, and restate
them as `--ui-*` overrides if they should keep applying.

### `--ui-*` (hermes/tokens.css)

| Token | Default, light | Default, dark | Used by |
|---|---|---|---|
| `--ui-surface` | `var(--color-bg)` (white) | same token (black) | Workspace: chip, card and column washes, tab bar, sheet, menus, panel, selected filters' text; the tints below |
| `--ui-surface-raised` | `var(--color-message-bg)` | same token | Workspace: notices, counts, list items, comments, the sheet's report; voice page: the assistant's bubbles |
| `--ui-text` | `var(--color-text)` | same token | Workspace: text, selected toggles and filters, the Report disc; voice: the recording button, error tip |
| `--ui-text-reversed` | `var(--color-text-reversed)` | same token | Voice: text on `--ui-text` fills; via `--ws-on-cc`: card numbers, chip numbers, selected column tabs |
| `--ui-text-muted` | `color-mix(in srgb, var(--ui-text) 60%, transparent)` | follows `--ui-text` | Workspace: hints, empty states, field labels, tabs, "loading", panel subtitle, chat "via" badge |
| `--ui-border` | `var(--color-border)` | same token | Workspace: autonomy table rows; voice: the control bar's top line |
| `--ui-border-strong` | `var(--color-border-dark)` | same token | Workspace: tab bar top line, tags, filters, "⋯" menu, answered drafts, panel edge |
| `--ui-border-stronger` | `var(--color-border-darker)` | same token | Voice: level meter track, Hermes question bubble, confirm box |
| `--ui-accent` | `var(--color-link)` | same token | Workspace: focus ring of tag and severity toggles |
| `--ui-accent-tint` | `var(--color-selected)` | same token | Voice page: the person's own bubbles |
| `--ui-success` | `var(--color-positive)` | same token | Voice page: the "sent" check |
| `--ui-warning` | `var(--color-alert)` | same token | Workspace: the notice's side bar (Fizzy down, waiting) |
| `--ui-danger` | `var(--color-negative)` | same token | Errors (Workspace statuses, failed log lines), recording dot, level and ring, voice notices; `--ws-sev-critical` |
| `--ui-on-danger` | `oklch(100% 0 0)` | `oklch(20% 0.0195 232.58)` | Text on a critical severity pill (`--ws-on-critical`) |
| `--ui-success-tint` | `color-mix(in oklch, var(--ui-success) 14%, var(--ui-surface))` | follows | Not used yet |
| `--ui-warning-tint` | `color-mix(in oklch, var(--ui-warning) 14%, var(--ui-surface))` | follows | Not used yet |
| `--ui-danger-tint` | `color-mix(in oklch, var(--ui-danger) 14%, var(--ui-surface))` | follows | Voice page: notice background |
| `--ui-ai` | `oklch(40% 0.12 50)` | `oklch(86% 0.103 90)` | AI text (Hermes drafts and proposals, "✦" labels, direct "via" badge), via `--ws-ai` |
| `--ui-ai-line` | `oklch(74% 0.184 70)` | `oklch(62.1% 0.146 70)` | AI dashed borders, "✦" marks, the AI count badge, via `--ws-ai-line` |
| `--ui-ai-tint` | `oklch(96% 0.034 100)` | `oklch(27% 0.045 75)` | AI backgrounds (draft boxes, draft items, direct badge), via `--ws-ai-soft` |
| `--ui-on-ai-line` | `oklch(26% 0.018 40)` | same | Text on the AI count badge |
| `--ui-radius-sm` | `0.25em` | – | Chip, card, card number |
| `--ui-radius-md` | `0.5em` | – | Board columns, "⋯" menu, the sheet's report |
| `--ui-radius-bubble` | `0.66em` (upstream's bubble) | – | Drafts, notices, list items, comments; voice bubbles |
| `--ui-radius-lg` | `0.8em` | – | Voice page notice and confirm boxes |
| `--ui-radius-pill` | `2em` | – | Severity pill, counts, tags, column tabs, filters |
| `--ui-radius-round` | `50%` | – | Dots, the Report disc, voice buttons and icons |
| `--ui-shadow` | `0 0 0 1px oklch(0% 0 0 / 5%), 0 0.2em 0.2em oklch(0% 0 0 / 5%), 0 0.4em 0.4em oklch(0% 0 0 / 5%)` | `0 0 0 1px oklch(100% 0 0 / 10%)` | Chips, cards, "⋯" menu (via `--ws-shadow`) |
| `--ui-shadow-lifted` | `0 0.4rem 0.9rem -0.25rem oklch(0% 0 0 / 35%)` | same | The tab bar's Report disc |
| `--ui-shadow-sheet` | `-0.5em 0 2em oklch(0% 0 0 / 18%)` | same | The card sheet overlay |
| `--ui-gutter` | `calc(var(--inline-space) + 1vw)` | – | Side padding of Home, the board, the sheet overlay, the room panel |
| `--ui-rail` | `max(5vw, 4rem)` | – | Wide screens: the tab rail's width and the pages' start padding beside it |

### `--ws-*` (top of workspace.css)

| Token | Default |
|---|---|
| `--ws-sev-critical`, `--ws-on-critical` | `var(--ui-danger)`, `var(--ui-on-danger)` |
| `--ws-ai`, `--ws-ai-line`, `--ws-ai-soft` | `var(--ui-ai)`, `var(--ui-ai-line)`, `var(--ui-ai-tint)` |
| `--ws-on-cc` (text on a column colour) | `var(--ui-text-reversed)` |
| `--ws-shadow` | `var(--ui-shadow)` |
| `--ws-sev-low`, `--ws-sev-medium`, `--ws-sev-high` | own `oklch()` values, light and dark |
| `--ws-col-new`, `-doing`, `-later`, `-done`, `-tan`, `-yellow`, `-aqua`, `-violet`, `-purple`, `-pink` | own `oklch()` values, light and dark (Fizzy's column colours); `--ws-cc` is set per element from them (`.ws-cc--*`) |
| `--ws-tabbar-height`, `--ws-panel-width` | layout, not colour |

### Still literal (selector overrides, or new tokens, when the design needs them)

- Font sizes and weights: no type tokens yet (about 36 sizes in `workspace.css`, 7 in `hermes.css`).
- A few radii that match no step: `0.3em` (via badges), `0.75em` (voice-note error tip), `0.9em`
  (the live-report button on touch screens), `1rem` (level meter).
- Gaps and paddings inside components (`0.3em`–`0.9em`), the `100ch` and `40rem` breakpoints
  (media queries can't read variables).
- `color-mix()` washes of a column colour (`--ws-cc`) and the muted variants at 55 %, 65 % and 70 %
  (they mix `--ui-text`, so they follow it).
- Icon colours: black SVG `<img>` with `filter: invert()` in dark mode.
