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
| `waveform.svg` | New (Hermes fork): the composer's link to the live voice report (Gemini) |
