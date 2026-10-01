# Hermes fork: the theme layer

The product's own look, "MeshDuty", and its plumbing (the "theme layer", option B of the UI redesign
handbook in the Hermes repo, `docs/ui-redesign/`; the style guide and the owner's decisions on it are
in its chapter 08). `theme.css` carries the look: fonts, a light and a dark palette, a Light / Dark /
System switch, and the restyle of upstream's and the fork's screens, in CSS only (see "The look"
below). The plumbing gives the designer:

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
| 7 | The light/dark switch's inline `<script>` (`THEME_SCRIPT`, `crates/views/src/theme_script.js`), with the theme only | the head seam |

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

## The kill switch: `CAMPFIRE_THEME`

`on` (the default) or `off` (`false`, `0`, `no`); any other value stops the app at boot. `off`
leaves out `theme.css`, the light/dark script and the profile's Light / Dark / System control
(nothing else): the tokens and the fork's stylesheets stay, since the voice features and the
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
looks it up by path: `#[template(path = …)]`, `{% include %}`, `{% extends %}`. One template is
shadowed (list and upstream blobs in `templates-hermes/SHADOWED.md`):

| Shadowed | Why |
|---|---|
| `users/profiles/show.html` | The theme's Light / Dark / System control, `{{ crate::hermes::color_scheme_switch(ctx)\|safe }}`, before the memberships. A copy of upstream's with that one call added on the line of the memberships `<div>`: it renders `""` until the app installs the page assets with the theme on, so the goldens (`parity_a.rs`, `users_profiles_show`) still compare upstream's bytes and need no allowlist |

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
followed `prefers-color-scheme` only; with the theme, it follows the switch (see "The look").

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

The tables above are the defaults (theme off). With the theme, `theme.css` maps every `--ui-*`
and the `--ws-*` colours onto its palette ("The look", "Mappings").

### Still literal (selector overrides, or new tokens, when the design needs them)

- Font sizes and weights in `workspace.css` and `hermes.css` (about 36 and 7): the theme overrides
  the ones the style guide names (titles, overlines, pills, counts, record IDs) by selector.
- A few radii that match no step: `0.3em` (via badges), `0.75em` (voice-note error tip), `0.9em`
  (the live-report button on touch screens), `1rem` (level meter).
- Gaps and paddings inside components (`0.3em`–`0.9em`), the `100ch` and `40rem` breakpoints
  (media queries can't read variables).
- `color-mix()` washes of a column colour (`--ws-cc`) and the muted variants at 55 %, 65 % and 70 %
  (they mix `--ui-text`, so they follow it).
- Icon colours: black SVG `<img>` with `filter: invert()`; with the theme, from `--icon-invert`.

## The look: MeshDuty (`theme.css`)

The owner's style guide (Hermes repo, `docs/ui-redesign/design/`, fit and decisions in chapter 08),
restyling what exists: nothing the guide shows that the app lacks (guest concierge, SOPs, team
spaces, My tasks), no markup or text change in this step. The renaming (MeshDuty, Sky) and the
voice page's English come separately. `theme.css` is laid out in five parts: fonts, palette (light,
dark twice), mappings, upstream components grouped by the upstream file they override, the fork's
screens.

### Fonts

Self-hosted under `crates/assets/overrides/hermes/fonts/` (WOFF2, Latin and Latin Extended
subsets, from Google Fonts; SIL Open Font License 1.1, licence texts next to them). No request to
Google at runtime. `url("fonts/…")` in `theme.css` resolves to `hermes/fonts/…` and is rewritten to
the digested URL by the asset pipeline (the font's bytes are part of `theme.css`'s digest).
`font-display: swap`; fallbacks are the system stacks.

| Token | Family | Files | Used for |
|---|---|---|---|
| `--font-serif` | Libre Caslon Text 400, 700 | 4 (static) | Titles: the page/room title pill, Home section titles, card and chip titles, the sheet and board titles, panel headings and legends (sign-in, account), Workspace item titles, the voice page's status |
| `--font-sans` (= upstream's `--font-family`) | Instrument Sans 400–700 | 2 (variable) | Everything else |
| `--font-mono` | JetBrains Mono 100–800 | 2 (variable) | Ticket numbers (`#412`, `No. 412`), message timestamps, counts, timers and durations, `code`, `pre`, the version badge |

Sizes stay relative (`rem`/`em`, upstream's base) so text follows the person's setting; form
fields keep upstream's 16px floor (`inputs.css`, iOS zoom). Overlines (day separators, field
labels, the draft hint, column headings) are 0.68rem, 600, caps, 0.16em tracking.

### Palette

Light is the guide's palette with chapter 08's contrast fixes (`--danger` darker; voice buttons
and badges filled with terracotta 600 instead of 500/400; overlines and text on canvas in
terracotta 700 / ink 2; timestamps in `--ink-muted`, `--ink-subtle` kept for placeholders and
decoration). Dark is chapter 08's draft (the designer can adjust it). The theme adds a few roles
(the last block) so components don't hard-code a palette step.

| Token | Light | Dark | Use |
|---|---|---|---|
| `--forest-900` | `#1f3b34` | `#16302a` | sidebar, primary buttons, my messages |
| `--forest-700` | `#2d4c44` | `#23423a` | active and hovered sidebar items |
| `--sage-300` | `#9fb2a9` | `#9fb2a9` | muted text on the sidebar |
| `--sidebar-ink` | `#e9e4d8` | `#e9e4d8` | text on the sidebar |
| `--accent` | `#c2613f` | `#b0512f` | terracotta 500: active underline, unread dots, pulsing outlines (never under text) |
| `--accent-400` | `#c9694a` | `#c9694a` | logo mark only (large text) |
| `--accent-600` | `#b0512f` | `#e59a7c` | links, text actions, focus rings |
| `--accent-700` | `#8f3f22` | `#f0b59d` | link hover, high severity text |
| `--accent-200` | `#e59a7c` | `#e59a7c` | accent on the dark sidebar |
| `--accent-tint` | `#f7e3d9` | `#3a2219` | high severity pill, mention highlight, selection, mic halo |
| `--bg-canvas` | `#e9e4da` | `#121715` | behind the app (desktop rail and gutter) |
| `--bg-surface` | `#f4f0e8` | `#181e1b` | app body (`--color-bg`) |
| `--bg-raised` | `#fdfbf6` | `#202723` | cards, bubbles, inputs, sheets, menus |
| `--bg-sunken` | `#ece6db` | `#151a18` | board columns, the sheet's report, shaded boxes |
| `--bg-chip` | `#efeae0` | `#29312c` | neutral chips, record IDs, counts |
| `--border` | `#e3dccd` | `#36403a` | dividers, chips' outline |
| `--border-subtle` | `#e7e1d4` | `#2e3732` | card and bubble outlines |
| `--border-faint` | `#eee7da` | `#262e2a` | lines inside cards |
| `--border-strong` | `#cdc5b4` | `#4f5a53` | inputs and secondary buttons (theme addition) |
| `--ink` | `#1f2522` | `#ece7dc` | text |
| `--ink-2` | `#4b4f49` | `#c9c4b8` | secondary text, text on chips and canvas |
| `--ink-muted` | `#6b6f68` | `#a6a99f` | meta, timestamps, hints (on raised or surface only) |
| `--ink-subtle` | `#8b8e86` | `#8f9289` | decoration only in light |
| `--ink-disabled` | `#b9b5aa` | `#4d534e` | disabled fills |
| `--ai` | `#4a7f8a` | `#6fa3ae` | Sky (Hermes): the ✦ mark |
| `--ai-ink` | `#2f5f6b` | `#9cc6cf` | Sky's labels, the "to confirm" count |
| `--ai-tint` | `#e3ecef` | `#1e3135` | Sky's backgrounds ("direct" badge) |
| `--ai-border` | `#9cb7bd` | `#4d7a83` | Sky's dashed suggestion outline |
| `--success`, `--success-tint` | `#2f6a4b`, `#e1eee5` | `#86c6a2`, `#1b3125` | done, ✓, OK statuses |
| `--warning`, `--warning-tint` | `#8a6417`, `#f6ecd2` | `#e2b95f`, `#372d14` | medium severity, the Workspace's notices |
| `--danger`, `--danger-tint`, `--danger-border` | `#9a4628`, `#fbeee8`, `#e7b9a5` | `#f0a083`, `#3a2018`, `#7a3f2a` | errors, failed states |
| `--primary`, `--primary-hover`, `--on-primary` | `#1f3b34`, `#2d4c44`, `#fdfbf6` | `#335c50`, `#3a6457`, `#ece7dc` | primary buttons (`.btn--reversed`), selected toggles and filters |
| `--danger-fill` | `#9a4628` | `#a3472a` | destructive buttons, the critical pill (white text) |
| `--voice-fill` | `#b0512f` | `#b0512f` | voice buttons, the Report disc (white text) |
| `--bubble-me`, `--bubble-me-ink`, `--bubble-me-muted`, `--bubble-me-link` | `#1f3b34`, `#f4f0e8`, `#b9c7c0`, `#f0b59d` | `#23423a`, `#ece7dc`, `#a9bab2`, `#f0b59d` | my messages |
| `--overline-ink` | `#8f3f22` | `#e59a7c` | overlines, day separators, the voice transcript's roles |

The same blocks hold the Workspace's column tones (`--ws-col-*`, Fizzy's column colours toned to
the palette, used as dots), upstream's code colours (`code.css`'s own light and dark values),
`--icon-invert` (0 light, 1 dark: icons are black `<img>`), the shadows (`--shadow-sm`,
`--shadow-lg`), the scrollbar colour and `color-scheme`.

**Upstream's `--lch-*`** are set in each block as the exact OKLCH triplets of these colours (each
line names its hex and token in a comment): black = `--ink`, white = `--bg-surface`, gray /
gray-dark / gray-darker = `--border-subtle` / `--border` / `--border-strong`, blue = `--accent-600`,
blue-light = `--accent-tint`, blue-dark = `--accent-200` (dark: `--accent-700`), orange =
`--accent`, red = `--danger`, green = `--success`; `--lch-always-black` stays (shadows). Upstream's
direct `oklch(var(--lch-…) / a)` uses (nav fade, lightbox, signup) follow.

**To change a colour:** edit the light block and both dark blocks (they must stay identical), and
if the token feeds an `--lch-*` line, recompute its triplet (any OKLCH converter;
`crates/assets/tests/theme.rs` fails with the expected value otherwise).

### Mappings (the same in light and dark)

- `--color-*`: bg = surface, message-bg = raised, text = ink, text-reversed = raised (default button
  fill, text on dark), link = accent 600, border / -dark / -darker = border-subtle / border /
  border-strong, selected = accent tint, selected-dark = accent 600 (focused field border), alert =
  accent, negative = danger, positive = success. `--font-family` = `--font-sans`;
  `--btn-border-radius` and `--input-border-radius` = `--radius-md` (`--radius-xl` on touch
  screens); `--input-background` = raised.
- `--ui-*`: surface = raised, surface-raised = surface (one step back inside a raised sheet), text
  = ink, muted = ink muted, borders = border-subtle / border / border-strong, accent = accent 600,
  AI = Sky's teal (`--ai-ink`, `--ai-border`, `--ai-tint`), radius-bubble = 14px, shadows from
  `--shadow-sm` / `--shadow-lg`.
- `--ws-*`: severity from `--sev-*` (low: ink 2 on chip; medium: warning on its tint; high: accent
  700 on accent tint; critical: white on `--danger-fill`), AI from Sky's teal.
- Spacing `--space-1…12` (4px base), radius `--radius-xs…pill` (4/8/10/12/14/999px) and
  `--tracking-overline` are the guide's names; upstream's own spacing tokens are left alone.

### Light / Dark / System

The person chooses on their profile page (`/users/me/profile`, "Theme": Light, Dark, System). The
choice is per browser, in `localStorage["hermes-theme"]` (`"light"` or `"dark"`; "System" removes
it), every access in `try/catch` (private windows: System).

- **Before the first paint**: the head seam inlines `THEME_SCRIPT` (`crates/views/src/theme_script.js`)
  after the theme's `<link>`; it sets `data-theme="light"` or `"dark"` on `<html>`, or no attribute
  for System. It also listens for the radios' `change` (saves and applies at once), re-applies on
  `turbo:load` / `turbo:render` (in case a render touched `<html>`), checks the right radio, and
  follows a change made in another tab (`storage` event).
- **CSS**: light on `:root`; dark under `:root[data-theme="dark"]` and, for System, under
  `@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { … } }` (same values,
  kept identical by `tests/theme.rs`). An explicit Light wins over a dark phone; Dark wins over a
  light one. Only the palette changes between modes: every upstream rule that had its own dark
  variant (13 blocks in 9 files: icon inversion, `.shadow`, the mention tint, code colours) and the
  fork's (`hermes.css`, `workspace.css`) is restated in `theme.css` from `--icon-invert` and the
  palette, at the same specificity and later, so it no longer depends on the media query.
- **Why the profile page**: it is where each person's own settings are (name, avatar,
  notifications), it exists with the Workspace on or off, and it is reachable on phones (sidebar →
  your avatar) and desktops. The Workspace's tab bar would only exist with the Workspace on and has
  no room for a three-way control. The cost is one shadowed template (`users/profiles/show.html`,
  one seam call).
- `CAMPFIRE_THEME=off` drops the script and the control too (`color_scheme_switch` renders `""`).
- Not followed: the browser chrome's `theme-color` metas and the manifest colours stay upstream's
  `#ffffff` / `#000000` by `prefers-color-scheme` (template change, with the renaming step).

### What changed on screen

Upstream screens (by upstream file, CSS only): page background canvas, app body surface; the room
and page title pill is a forest primary pill in Caslon; buttons r10 (r14 on touch), primary forest,
secondary raised with a strong border, destructive filled; inputs raised with the strong border
and a terracotta focus border; **the sidebar is dark forest** (tokens scoped to `#sidebar`: rooms
as nav items, white icons, terracotta unread dots, forest 700 on hover); bubbles raised with a
subtle border, r14; **my own messages are forest bubbles** (upstream's `.message--me`, added in
the browser by the message formatter, so the cached message HTML is untouched; the "⋯" menu, the
edit form and emoji-only messages keep the page's colours); timestamps in mono, muted; day
separators as terracotta overlines; panels (sign-in, account, profile) raised cards on wide
screens; menus, autocomplete, link previews, dialogs (no more rainbow backdrop), lightbox, flash,
code, scrollbars.

The fork's screens: card chips as small record cards (mono `#412`, Caslon title, severity and
state pills, the column colour as the state's dot); severity as tinted pills; Sky's drafts and
proposals dashed teal with a teal overline, solid with a green ✓ once filed, neutral once
answered; Home sections in Caslon, raised ticket cards with a mono record ID, counts as chips (the
"to confirm" one teal); the tab bar raised with a terracotta underline under the current tab and
a terracotta Report disc; board columns sunken, column tabs and filters with a primary selected
state; the card sheet and room panel raised; the voice buttons in the composer and the voice
page's big button terracotta (hang-up stays destructive), the transcript's lines as bubbles (mine
forest, Sky's questions dashed teal).

Small fixes made on the way (CSS, existing markup): the Workspace's step checkboxes and the
autonomy table's radios were invisible (upstream's `appearance: none` on every input) and are
native again; the board's severity filters and the sheet's departments were laid out as a column
by upstream's `fieldset` rule and are rows; the board and (on phones) Home start below the fixed
title pill instead of under it; the Workspace pages' title pill no longer slides over the back
button on wide screens; the chip's number no longer wraps one digit per line on phones.

Not done, and why:

- Sky's own chat messages aren't tinted teal: the cached message HTML says nothing about bots
  (only `data-user-id` and the name), and matching a name would break with the renaming.
- The `40rem` voice-page breakpoint (owner decision 11, "same as the rest") and the guide's px type
  scale: layout changes, left for later.
- Icons stay black `<img>` inverted to white (owner decision 7 still open): no terracotta icons.

### Contrast (WCAG 2.x, computed from the hex values)

| Pair | Light | Dark |
|---|---|---|
| Body text, `--ink` on surface | 13.73 | 13.73 |
| Text in cards and bubbles, `--ink` on raised | 15.09 | 12.38 |
| Meta, timestamps, hints, `--ink-muted` on raised | 4.95 | 6.39 |
| Meta on the page, `--ink-muted` on surface | 4.51 | 7.09 |
| Record IDs, low severity, `--ink-2` on chip | 6.97 | 7.69 |
| Tab labels on the desktop rail, `--ink-2` on canvas | 6.59 | 10.41 |
| Links, `--accent-600` on surface / raised | 4.55 / 5.00 | 7.45 / 6.72 |
| Overlines, day separators, `--overline-ink` on surface | 6.37 | 7.45 |
| Primary button, `--on-primary` on `--primary` | 11.71 | 6.11 |
| Voice buttons, Report disc, white on `--voice-fill` | 5.17 | 5.17 |
| Critical pill, destructive button, white on `--danger-fill` | 6.40 | 6.01 |
| Medium severity, `--warning` on its tint | 4.56 | 7.33 |
| High severity, `--accent-700` on `--accent-tint` | 5.84 | 8.29 |
| Sky's labels, `--ai-ink` on raised | 6.84 | 8.28 |
| "To confirm" count, raised on `--ai-ink` | 6.84 | 8.28 |
| Success / danger text on their tints | 5.35 / 5.64 | 7.02 / 7.19 |
| My messages: text / meta / links on `--bubble-me` | 10.66 / 6.92 / 6.81 | 8.91 / 5.42 / 6.17 |
| Sidebar: text / sage on forest 900; text on forest 700 | 9.55 / 5.43; 7.43 | 11.11 / 6.32; 8.66 |
| Focus ring (non-text), `--accent-600` on surface | 4.55 | 7.45 |
| Field and secondary button outline (non-text), `--border-strong` on raised | 1.66 | 2.12 |

Every text pair is ≥ 4.5:1. The field outline is the guide's light look and stays under 3:1: fields
are identified by their label, placeholder and position, and get a terracotta border and ring on
focus; strengthen `--border-strong` if a stricter reading of WCAG 1.4.11 is wanted. Focus:
upstream's `:focus-visible` outline (accent 600) on buttons and fields, plus a 2px ring on links,
checkboxes, radios and selects, and on the Workspace's toggle pills and the theme radios. Motion:
upstream's reset stops animations under `prefers-reduced-motion`; the theme's own transitions
(cards, chips, the theme radios) are turned off there too.
