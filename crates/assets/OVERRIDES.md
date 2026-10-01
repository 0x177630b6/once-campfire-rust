# Frontend overrides

Files in `overrides/` shadow the reference app's assets of the same logical path (the path under
`app/javascript`, `app/assets/*` or a vendored gem's asset directory), so the Rust app can change its
frontend without editing the `reference/` submodule. `build.rs` puts that directory first on the load
path. Files the reference doesn't have are added; a new one under a `pin_all_from` directory
(e.g. `controllers/`) is pinned in the import map like the reference's own files.

| File | Differs from the reference by |
|---|---|
| `models/file_uploader.js` | No `X-CSRF-Token` header: pages carry no CSRF token (forgery protection is by `Sec-Fetch-Site`) |
| `controllers/copy_to_clipboard_controller.js` | A `url` value: a path, copied as an absolute URL against the page, so the cached message markup that carries it doesn't depend on the request's host |
| `lib/autocomplete/base_autocomplete_handler.js` | Asks for JSON (`Accept: application/json`). The reference passes `{ as: "json" }`, a `@rails/request.js` option, to plain `fetch`, gets HTML and never shows the new-ping suggestions |
| `install-edge.svg` | New: a copy of `external/install-edge.svg` where `pwa/_install_instructions` looks for it. Rails can't find it, so Edge gets a 500 on profile and room pages |
| `controllers/voice_controller.js` | New (Hermes fork): the live voice incident report's Stimulus controller (docs/hermes-gemini-live.md) |
| `voice/pcm-worklet.js` | New (Hermes fork): the AudioWorklet that controller loads from its digested URL; not pinned |
| `microphone.svg` | New (Hermes fork): the composer's record-a-voice-note button |
| `controllers/voice_note_controller.js` | New (Hermes fork): records a voice note (`MediaRecorder`) from the composer and hands the file to the composer's attachment path (`drop-target:drop`, then Send) |
| `headset.svg` | New (Hermes fork): the composer's link to the live voice report |
| `phone-hangup.svg` | New (Hermes fork): the live voice page's hang-up button |
| `controllers/voice_player_controller.js` | New (Hermes fork): shows a voice note's duration next to its inline player once the browser knows it |
| `hermes/workspace.css`, `hermes/workspace.js`, `hermes/workspace_logic.js`, `hermes/home.svg`, `hermes/board.svg` | New (Hermes fork): the Duty Manager Workspace (docs/hermes-workspace.md): chips, draft buttons, tab bar, Home, board, card sheet, room panel, settings and Hermes tab styles; the page script (a plain module, not pinned: chips refresh, draft and proposal buttons, card sheet overlay, board, room panel, new-card form, settings form, undo) and its page-independent logic (`workspace_logic.js`, a module loaded first, tested by `node --test crates/assets/tests/js/*.test.mjs`); the tab bar's Home and Boards icons. The scripts are loaded by the workspace's tab bar partial and the stylesheet from the layout's head seam (`campfire_views::hermes::head_tags`), both only while the workspace is on |
| `hermes/hermes.css` | New (Hermes fork): every style of the voice features (composer buttons and recording bar, inline audio player, live voice page). `build.rs` leaves `hermes/` out of `stylesheet_link_tag :all`, so that list keeps the reference's exact `<link>` tags; the layout's head seam (`campfire_views::hermes::head_tags`) links it on every page |
| `hermes/tokens.css` | New (Hermes fork): the fork's semantic design tokens (`--ui-*`, docs/hermes-theme.md) that `hermes.css`, `workspace.css` and the theme build on; linked first by the head seam |
| `hermes/theme.css` | New (Hermes fork): the product theme, MeshDuty (docs/hermes-theme.md): fonts, the light and dark palette (upstream's `--lch-*` as OKLCH triplets, `--color-*`, `--ui-*`, `--ws-*` mapped onto it), the Light / Dark / System switch's CSS, and the restyle of upstream's and the fork's screens by class name. Linked last by the head seam, after Custom styles; left out with `CAMPFIRE_THEME=off`. Its `url("fonts/…")` are digested like any asset URL. `tests/theme.rs` checks its palette blocks and fonts |
| `hermes/fonts/*.woff2`, `hermes/fonts/OFL-*.txt` | New (Hermes fork): the theme's self-hosted fonts (Libre Caslon Text 400/700, Instrument Sans and JetBrains Mono variable; Latin and Latin Extended subsets from Google Fonts) and their SIL Open Font License texts. Referenced only from `theme.css` |
