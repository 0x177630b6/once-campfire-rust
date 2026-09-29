# Hermes fork: live voice incident reports (Gemini Live)

An employee opens a room's voice page, talks an incident through with a Gemini Live voice
interviewer that asks only for what's missing, confirms the recap, and the report is posted **in the
room, as that employee**, with an @mention of the Hermes bot. The mention fires the bot's webhook
exactly as a typed @mention would, so the Hermes bridge and its `incident-report` skill take it
from there (Fizzy card etc.).

This is a fork-only feature: none of it exists in the reference app or upstream
`once-campfire-rust`. Everything is off unless `GEMINI_API_KEY` is set.

## Flow

```
Browser (Campfire session)                 Campfire (this fork)                    Gemini / Hermes
GET  /rooms/:id/voice ───────────────────▶ membership check, page
POST /rooms/:id/voice/token ─────────────▶ POST v1alpha/auth_tokens (x-goog-api-key)
     ◀── {token, ws_url, model, expires_at}: single use, 1 min to open, 30 min life,
         model/instructions/tools/transcription locked in the token
mic 16 kHz PCM ══WSS (BidiGenerateContentConstrained, {"setup":{}})══▶ Gemini Live
     ◀══ 24 kHz audio + input/output transcriptions
tool call submit_incident(...)
POST /rooms/:id/voice/report ────────────▶ message as Current.user, "@Hermes …"
                                            broadcast + deliver_webhooks_to_bots ──▶ bot webhook
     ◀── 201 {message_id, message_url}
```

The API key never reaches the browser. The browser only gets a single-use ephemeral token whose
`bidiGenerateContentSetup` fixes the model, the French interviewer instructions (with the room and
user names frozen in, as quoted data), the `submit_incident` declaration, audio transcription both
ways, session resumption and sliding-window context compression.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `GEMINI_API_KEY` | unset (feature off) | Gemini API key (paid tier: the free tier's terms exclude EEA/CH/UK end users and allow human review). Server-side only; never logged. |
| `GEMINI_LIVE_MODEL` | `models/gemini-3.8-live` | Live model; a bare name gets `models/` prepended. |
| `GEMINI_LIVE_VOICE_BOT` | unset | The bot the report @mentions: a bot **user id** (e.g. `3`) or its **exact name** (e.g. `Hermes`). It must be an active bot **member of the room**. Unset: the room's only active bot; a room with none or several answers 422 `bot_not_in_room`. |
| `GEMINI_LIVE_TOKENS_PER_HOUR` | `10` | Tokens one user may mint per rolling hour (in memory, per process) before 429. |
| `GEMINI_LIVE_EXTRA_INSTRUCTIONS` | unset | Text appended to the interviewer's system instruction (e.g. site-specific questions). |

Browsers only allow the microphone on a secure origin: serve Campfire over HTTPS (or localhost).

## Routes and contract

All three answer 404 while the feature is off, run `ApplicationController`'s chain (session
cookie only, bots denied, `Sec-Fetch-Site` forgery protection on the POSTs, so same-origin `fetch`
needs no token) and `RoomScoped#set_room` (404 unless the user is a member of the room). They live in
a separate route table tried after the Rails one (`controllers::HERMES_ROUTES`), so the Rails table
still matches `bin/rails routes`.

### `GET /rooms/:room_id/voice`

HTML page in the application layout (Turbo-Frame requests get the frame layout). Root element:

```html
<section class="voice" data-controller="voice"
  data-voice-token-url-value="/rooms/:room_id/voice/token"
  data-voice-report-url-value="/rooms/:room_id/voice/report"
  data-voice-worklet-url-value="/assets/voice/pcm-worklet-<digest>.js"
  data-voice-room-url-value="/rooms/:room_id"
  data-voice-room-name-value="<room display name>">
  <button data-voice-target="toggle" data-action="voice#toggle">Démarrer</button>
  <p data-voice-target="status">…</p>
  <div data-voice-target="transcript"></div>
  <div data-voice-target="result" hidden></div>
</section>
```

The worklet URL is the Propshaft-digested path of `voice/pcm-worklet.js`, resolved through the asset
manifest (`campfire_assets::try_asset_path`); it's served like every digested asset
(`text/javascript`, immutable), which `audioWorklet.addModule(url)` accepts.

### `POST /rooms/:room_id/voice/token`

No body needed. `200`:

```json
{"token": "auth_tokens/…",
 "ws_url": "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1alpha.GenerativeService.BidiGenerateContentConstrained",
 "model": "models/gemini-3.8-live",
 "expires_at": "2026-09-29T12:30:00Z"}
```

The browser connects to `ws_url?access_token=<token>` and sends `{"setup":{}}`. Errors are JSON
`{"error": code, "message": French text}`: `429 rate_limited`; `502 upstream_error` or
`502 upstream_timeout` (the whole upstream call is capped at 10 s).

Upstream call: `POST https://generativelanguage.googleapis.com/v1alpha/auth_tokens`, header
`x-goog-api-key`, body `{"uses":1, "expireTime": now+30min, "newSessionExpireTime": now+1min,
"bidiGenerateContentSetup": {...}}`; the reply `{"name": "auth_tokens/…"}` is the token.
Checked against the live API on 2026-09-29 (200 with that shape, including the `enum` in the
function schema).

`submit_incident` parameters (all strings): `title` and `summary` (required), `what_happened`,
`location`, `occurred_at`, `people_involved`, `injuries`, `actions_taken`, `severity`
(`low` | `medium` | `high` | `critical`).

### `POST /rooms/:room_id/voice/report`

JSON body: the `submit_incident` arguments plus an optional `transcript` string. Each field is cut
at 2 KB and the transcript at 20 KB (cut text ends with `…`, and the transcript heading says
"(tronquée)"); an unknown `severity` is dropped. `201 {"message_id": 123, "message_url":
"https://host/rooms/:room_id/@123"}`. Errors: `422 invalid_report` (no title or summary),
`422 bot_not_in_room`.

The message is created **as the current user** through `MessagesController#create`'s own path
(`create_message` → `broadcast_create` → `deliver_webhooks_to_bots`), so it's canonicalized,
stored, broadcast, pushed and delivered exactly like a typed message. Its body, every value
HTML-escaped (the text comes from a model and a browser):

```html
<p><action-text-attachment sgid="<bot's attachable sgid>" content-type="application/vnd.campfire.mention"></action-text-attachment> Compte rendu d’incident dicté en direct (voix), confirmé par l’auteur.</p>
<h3>title</h3>
<p><strong>Résumé :</strong> summary</p>
<ul><li><strong>Ce qui s’est passé :</strong> … </li> … <li><strong>Gravité :</strong> élevée (high)</li></ul>
<p><strong>Transcription de l’entretien :</strong></p>
<blockquote>line<br>line…</blockquote>
```

Only tags the display sanitizer (`ContentFilters::SanitizeTags`) keeps are used (`<details>` is not
one of them, hence the blockquote). Missing fields read *non précisé*.

### Why the bot's webhook fires (the plan's open point)

In a shared (non-direct) room, `deliver_webhooks_to_bots` calls only the *mentioned* active bots:
`message.mentionees` = the `User` attachables of the stored body (`campfire_richtext::mentioned_users`,
verified SGIDs only) that are members of the room. The server writes the same
`<action-text-attachment sgid=… content-type="application/vnd.campfire.mention">` the composer
inserts, with the bot's `attachable_sgid` (the one the mention autocomplete hands out), and the
body goes through the same canonicalization, so the mention is recognized like a human's.
`controllers/voice/tests.rs` (`a_report_mentions_the_bot_whose_webhook_then_fires_in_a_shared_room`)
proves it end to end: the report in *All Talk* (a closed room) makes Bender's webhook receive the
message. Consequence: **the bot must be a member of the room**, or nothing can be delivered (hence
the 422 rather than a silent message).

## Composer buttons and voice notes

The room composer gets up to two buttons after the attachment (paperclip) button, both from
`hermes/_composer_buttons.html`:

- **Voice note** (microphone, always on: `ShowView::voice_note`, set by `rooms#show`). Tap to record,
  tap again to stop and send; the bin discards; a red "● 0:07" timer runs meanwhile; recording stops
  and sends by itself at 5 minutes. `voice_note_controller.js` records with `MediaRecorder`
  (`audio/webm;codecs=opus`, else `audio/mp4` for Safari, else `audio/ogg;codecs=opus`, else the
  browser's default) and names the file `note-vocale-YYYYMMDD-HHMMSS.webm|m4a|ogg` with a plain
  `audio/*` type. The file goes through the composer's own attachment path: the controller fires
  `drop-target:drop` on `window` (what a file dropped on the room fires, which `composer#dropFiles`
  handles) and clicks the composer's Send button, so the upload, its pending bubble, the
  `client_message_id` and the Turbo Stream answer are those of a picked file. Like Send, that also
  sends any text typed in the composer. The button stays hidden where the browser can't record; on
  plain HTTP it shows and explains that HTTPS is needed.
- **Live report** (waveform, only when `ShowView::voice_path` is set, i.e. `GEMINI_API_KEY`): a
  link to `/rooms/:id/voice`, out of the composer's turbo frame (`data-turbo-frame="_top"`). It
  replaces the room nav's mic button of v0.1.1-hermes.2.

Audio attachments (`audio/*`, e.g. voice notes, whatever recorded them) render as
`<audio controls preload="none">` above the usual file link (`AttachmentPreview::Audio`,
`hermes::audio_preview`); the reference only shows the file link.

## Where the code is

| Path | What |
|---|---|
| `crates/campfire/src/config.rs` | `GeminiLiveConfig`, `ApiKey` (redacted `Debug`) |
| `crates/campfire/src/integrations/gemini_live.rs` | Token request body, system instruction, `submit_incident`, `HttpMinter` (the existing `integrations::net` HTTP/1.1 + rustls client, no new crate), `TokenMinter` trait (tests swap it), rate limiter |
| `crates/campfire/src/controllers/voice.rs` | The three actions, `IncidentReport` (caps, escaping, markup) |
| `crates/campfire/src/controllers/mod.rs` | `HERMES_ROUTES`, tried after the Rails table |
| `crates/campfire/src/app.rs` | `AppState::gemini_live` |
| `crates/views/src/hermes/`, `crates/views/templates/hermes/` | The page, the composer's voice buttons (`_composer_buttons.html`) and the inline audio player |
| `crates/views/templates/rooms/show/_composer.html` | The one upstream-template insertion: `hermes/_composer_buttons` after the attachment button |
| `crates/assets/overrides/controllers/voice_controller.js`, `voice/pcm-worklet.js`, `waveform.svg` | Frontend |
| `crates/assets/build/importmap.rs` | `pin_all_from` also picks up files the overrides *add* (otherwise `controllers/voice_controller` would never be pinned or registered) |

## Tests

- `integrations::gemini_live::tests`: request body shape, quoting of names, extra instructions, rate
  limit, and the HTTP minter against a fake server (path, `x-goog-api-key`, body; 403 and garbage
  replies are errors).
- `controllers::voice::tests`: feature off → 404 and no live button (the voice-note one stays);
  the page's data values and worklet URL (served as JavaScript); token route builds the locked setup through an injected
  minter, forgery protection, membership, 429, 502; report membership / validation / no-bot room;
  the bot-mention webhook proof above; report escaping and caps; route order.
- `crates/views/tests/hermes_views.rs`: the page renders; the composer's buttons are absent with both
  flags off (the goldens' input) and present, in place, with each on; audio attachments get a player.
- `crates/assets/tests/reference.rs`: the import map equals the reference's plus the added
  `controllers/voice_controller` pin.

The request-level tests need the parity seed (`parity/bin/seed build`) and skip without it.

## Privacy

Transcripts are personal data (GDPR): they end up in the room's message and in whatever the bot
files. Decide on retention, or drop the transcript from the message if a summary is enough.
