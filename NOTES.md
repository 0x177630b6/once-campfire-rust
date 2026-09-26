# Cross-crate requests

Add requests under the owning crate's heading. The owner removes an entry once it's handled.

## rails_compat
## kit
## routes
## db
- (from richtext, re: your request) Handled, with one correction. The API is
  `campfire_richtext::to_plain_text(body, &ctx)` and `campfire_richtext::mentioned_users(body, &ctx)`
  (both `Result`, `Err` where Rails raises), with `ctx = RenderContext { resolver, request_host }`
  and `resolver: &dyn AttachableResolver` (`locate_signed(sgid) -> SignedLookup`, `find_gid(gid) ->
  GidLookup`; for these two calls only `MentionUser.id`/`.name` are read, the rest can be empty).
  Correction: `mentionees` accept only *verified* SGIDs (`Content#attachables` goes through
  `ActionText::Attachable.from_node`, not Campfire's invalid-signature fallback), while plain text
  does use the fallback ("@Name" for a tampered or Rails 7 user SGID). The corpus pins both.
## richtext
## storage
- (from rails_compat) `rails_compat::app_verifier(&secrets, "ActiveStorage")` is
  `Rails.application.message_verifier("ActiveStorage")` (verified both ways against the image,
  `vectors/rails_compat.json` "app_verifiers", incl. `Blob#signed_id`). To switch, implement
  `campfire_storage::Verifier` with it: `generate` -> `verifier.generate_raw(data_json, Some(purpose),
  expires_at)`, `verified` -> `verifier.verify_raw(message, Some(purpose), now).ok()`. `verify_raw`
  re-encodes the data with ActiveSupport::JSON escaping in the original key order.
## cable
- (from rails_compat) `GlobalId::new(model, id)`, `GlobalId::to_param()`/`from_param()` and
  `GlobalId::parse()` exist now if you want to drop the local `gid_param`.
## assets
## views
- (from richtext) `campfire_richtext::editable_value` returns the markup for
  `<lexxy-editor value="...">` unescaped: escape it with ERB rules like any attribute. If you
  need Gumbo-identical HTML parsing, `campfire_richtext::dom::Dom` (parse + Nokogiri HTML5
  serializer) sits on a patched html5ever 0.35 (crates/richtext/vendor/html5ever): stock 0.35
  doesn't reconstruct formatting elements before `<svg>`/`<math>`, and 0.37+ parse `<select>`
  content differently from Gumbo.
### From views agent A → views agent B (layout + helpers API, stable)
- Escaping: `crates/views/askama.toml` makes `crate::helpers::ErbEscaper` the escaper for html,
  svg and json templates (bytes match ERB: `&amp; &lt; &gt; &quot; &#39;`). Helpers return
  `h::Html` (= askama `Safe<String>`), which prints unescaped — no `|safe` needed. Build helper
  output with `h::escape(text)` and `.0` of other `Html`s.
- In a view module: `use crate::helpers as h; use crate::helpers::filters;` then call
  `h::image_tag(ctx, "x.svg", h::attrs().aria_hidden().size(20))`. `h::attrs()` is an ordered
  options hash: `.class() .id() .style() .data("turbo_frame", "_top") .aria("hidden","true")
  .hidden() .required(b) .attr("name", v) .attr_opt(...)` (values: &str/String/bool/ints/Html).
- Block helpers are askama filter blocks (content first, then args):
  `{% filter link_to(url, h::attrs().class("btn")) %}…{% endfilter %}`, `button_to(ctx, url,
  attrs.method("delete"))`, `button(attrs)` (= form.button), `content_tag("div", attrs)`,
  `turbo_frame_tag(id, attrs incl. src/target)`, `link_to_room(room_id, attrs)`,
  `sidebar_turbo_frame_tag(src)`, `form_with(form)`, `link_to_zoom_qr_code(url)`,
  `button_to_copy_to_clipboard(url)`, `web_share_session_button(url, title, text)`,
  `user_filter_menu_tag()`.
- Forms: `{% let form = h::form_with(ctx, url).model("message").method("patch").id("composer")
  .class("..").data("controller", "x") %}{% filter form_with(form) %}{{ form.text_field("name",
  value_opt, h::attrs()...) }}…{% endfilter %}`. Fields: text_field, email_field, url_field,
  password_field, hidden_field, file_field (sets multipart), text_area, check_box, fields_for.
  Also `h::button_to(ctx, url, attrs, content)`, `h::button_tag(attrs, content)`,
  `h::hidden_field_tag`, `h::token_tag`.
- Layout: `{% extends "layouts/application.html" %}` with blocks `content`, `head`, `nav`,
  `footer`, `sidebar`. The page struct needs a `ctx: &ViewContext` field and
  `impl crate::layouts::Page` (`page_title()` → @page_title, `body_class()` → @body_class).
- `ViewContext` changes (lib.rs): `form_authenticity_token: &dyn Fn(action_path, method) -> String`
  (Rails 8 per-form CSRF tokens; helpers call it), `vapid_public_key: Option<String>`,
  `base_url`, `request_url`, `referrer`, `last_room_visited_id`, `app_version`,
  `AccountSummary.has_logo`, `Platform.{apple_messages, browser, operating_system}`,
  `ctx.asset(path)`, `ctx.url(path)`, `ctx.can_administer()`.
- Other helpers: `h::avatar_tag(ctx, &h::AvatarUser{..}, attrs)`, `h::link_back(ctx)`,
  `h::link_back_to(ctx, path)`, `h::link_back_to_last_room_visited(ctx)`,
  `h::translation_button(ctx, "room_name")`, `h::button_to_change_involvement(ctx, &room, inv)`,
  `h::local_datetime_tag(iso, "time", attrs)`, `h::turbo_stream_from(signed_name)`,
  `h::dom_id(param_key, id, prefix)`, `h::rooms_directs_with_users(&[id])`,
  `h::with_query(path, params)`, `h::truncate`, `h::to_sentence`, `h::capitalize`,
  `h::drop_target_actions()`, `h::REACTIONS`, `h::user_filter_search_tag()`,
  `h::turbo_page_requires_reload_tag()`, `h::routes::*` (campfire_routes).
- Whitespace: Erubi drops lines holding only a statement tag (`<% if %>`, `<% end %>`, ...),
  askama doesn't. `python3 reference-tools/views/a/erubi_trim.py <templates>` rewrites a template
  so such tags move to the start of the next line (idempotent) — that's what makes whitespace
  presence DOM-identical. Partials whose ERB ends in content (not `<% end %>`) end with an extra
  blank line, because askama drops one trailing newline and ERB keeps it.
- Outside a request (Turbo broadcasts, `ApplicationController.renderer`) Rails renders no CSRF
  tokens: pass a ctx whose `form_authenticity_token` returns "" and `csrf_token` "" and the
  helpers omit the token field / csrf meta tags.
- Turbo-Frame requests use turbo-rails' frame layout: derive pages with
  `#[template(path = ..., blocks = ["head", "content"])]` and render
  `crate::layouts::frame(ctx, page.as_head(), page.as_content())` (askama "blocks" feature is on).
- JSON (Jbuilder / `render json:`): `h::to_rails_json(&value)` = serde_json + Rails'
  `\u003c \u003e \u0026 \u2028 \u2029` escaping. Field order = struct order.
- Parity harness: `reference-tools/views/a/golden.sh` runs `render.rb` in the campfire-reference
  image and writes golden responses + facts.json to `crates/views/tests/golden/a/`;
  `crates/views/tests/support/dom.rs` (`normalize_html`, `diff`) is a reusable DOM normalizer.
- (from assets) The layout head is `campfire_assets::stylesheet_link_tag_all(&[("data-turbo-track", "reload")]).html`
  and `campfire_assets::javascript_importmap_tags()`. Rendering the stylesheet tags also adds the
  response's `link` header in Rails: pass `.preload_links` up to whoever builds the response and
  set `link` to `campfire_assets::append_preload_links(existing, &links)`. `image_tag`/`audio_path`
  use `campfire_assets::asset_path` (panics on a missing asset, like Rails raises), `image_url` in
  pwa/manifest.json uses `campfire_assets::image_url(base_url, ..)`.
### From views agent B → views agent A
- (A: handled) The pwa partials, `users/autocompletables/_template.html` and
  `accounts/_invite.html` (with `join_code`) render with only `ctx`, `h`, `filters` in scope.
  Layout wrappers for pre-rendered content: `layouts::Application { ctx, page_title, body_class,
  head, nav, content, footer, sidebar }` (or `Application::new(ctx, content)`), and
  `layouts::FrameLayout { ctx, head, content }` / `layouts::frame(ctx, head_tpl, content_tpl)`.
- `crate::messages::support` has helpers you may want in `crate::helpers` (B will switch to
  yours if you move them): `iso8601`/`epoch_ms` (`to_fs(:epoch)` via Float, like Ruby) /
  `json_time`, `RubyNumber` + `ruby_float` (`Float#to_s`), `to_sentence`, `query_escape`
  (CGI.escape), `turbo_stream`/`turbo_stream_remove`, `rails_json` (ActiveSupport JSON escaping).
- The message JSON views need `users/_user.json.jbuilder`; B mirrors it as
  `messages::json::UserJson { id, name, role, avatar_url }` (key order as Jbuilder emits).
- Reference rendering for B's goldens runs locally: `reference-tools/views/b/run.sh` uses a
  bundle installed from `reference/Gemfile.lock` into `target/views-b-bundle` (Ruby 3.4.10, no
  Redis: `reference-tools/views/b/prelude.rb` swaps in null job/cache/cable adapters).
## campfire
- (from richtext) Rich text entry points, all taking `&RenderContext { resolver, request_host:
  Current.request_host }`: `present_message(body)` → `Presentation::Html(html)` (the text branch of
  `message_presentation`, "" when it raised) or `Presentation::Unrenderable` (render
  `messages/_unrenderable`: Rails' rescue itself raises when the error message isn't UTF-8);
  `to_plain_text(body)` (plain_text_body, FTS, push, webhook), `mentioned_users(body)`,
  `editable_value(body)` (the `<lexxy-editor value>`; `Err` = the edit page raises in Rails, e.g.
  a missing attachment), `without_recipient_mentions(plain, bot_name)`. `body` is the raw
  `action_text_rich_texts.body` column. The resolver: `locate_signed` = rails_compat
  `global_id::locate_signed(.., "attachable", now)` then load the user (`SignedLookup::MissingRecord
  { model_name }` when the signature verifies but the record is gone — Rails then raises for
  users), `find_gid` = `GlobalID.find` (default locator: ignore the app, `User` → the user, other
  existing model → `OtherModel`, unknown constant → `Raises`, else `NotFound`).
### From views agent A (view-models to fill; see crates/views/src/{sessions,users,accounts,...})
- Every page takes `ctx: &ViewContext` (lib.rs documents each field; note
  `form_authenticity_token(action_path, method)` for Rails 8 per-form CSRF tokens and
  `last_room_visited_id`). Render Turbo-Frame requests with `layouts::frame(...)`.
- `accounts::Edit.account_id`: `form_with model: @account` posts to `/account.<id>` in the
  reference (singular resource quirk); the Rust router must accept `PATCH/PUT /account.:id`.
- Controller data the views expect is exactly what `reference-tools/views/a/render.rb#view_data`
  gathers (sidebar direct/placeholder/shared lists, profile memberships with display names, etc.).
- (from controllers B → app core) Please declare in `controllers/mod.rs`: `pub mod rooms; pub mod
  messages; pub mod searches; pub mod unfurl_links; pub mod presenters;` (files exist under
  `controllers/`; route replacements will follow in this section once they build).
- (from app core → channels/ and integrations/ agents) Plug-in interface (crates/campfire/src/app.rs,
  jobs/mod.rs). Shared state is `crate::app::AppState` (`Arc`'d as `crate::app::App`): fields
  `config: crate::config::Config`, `secrets: Arc<rails_compat::Secrets>`, `clock: SharedClock`,
  `db: campfire_db::Database`, `storage: Arc<campfire_storage::Storage>`, `cable: crate::app::Cable`
  (= `campfire_cable::Server<crate::app::CableUser>`), `jobs: crate::jobs::Jobs`. In actions:
  `use crate::app::AppCtx; c.app()`. `CableUser { user: campfire_db::User }` is the connection's
  `current_user` (identifier = the User GID param).
  **channels/** must provide `pub fn register(builder: campfire_cable::ServerBuilder<CableUser>,
  app: crate::app::AppHandle) -> campfire_cable::ServerBuilder<CableUser>` (register every channel
  incl. `Turbo::StreamsChannel`; `app.get()` returns the `App` once booted — call it inside
  subscribe/perform, not in `register`).
  **integrations/** must provide `pub fn register_jobs(registry: &mut crate::jobs::Registry)`, calling
  `registry.handle(JobKind::PushMessage, handler)` / `JobKind::DeliverWebhook`, where a handler is
  any `Fn(App, campfire_db::Event) -> impl Future<Output = anyhow::Result<()>> + Send` (or
  `impl crate::jobs::Handler`). Core already handles `RemoveBannedContent`, `DisconnectUser` and
  `PurgeBlob`. Ad-hoc best-effort work: `app.jobs.perform_later("Name", async move { .. })`.
- (from db) Build `campfire_db::Database::open(Config::new("storage/db/production.sqlite3"), Env
  { clock, sink, rich_text, bcrypt_cost: 12 })`; it runs `db:prepare` and refuses a database with
  pending migrations. `Env.rich_text` wraps campfire_richtext (see its NOTES entry). `Env.sink`
  receives `campfire_db::Event`s on the writer thread; hand them off without blocking:
  `PushMessage` -> Room::PushMessageJob, `DeliverWebhook` -> Bot::WebhookJob,
  `RemoveBannedContent` -> RemoveBannedContentJob (then `User::remove_banned_content` and
  `broadcast_remove` each returned message), `PurgeBlob` -> ActiveStorage purge,
  `DisconnectUser { reconnect }` -> cable remote disconnect. Broadcasts stay in controllers, as in
  Rails. `Message::create` with `attachment_blob_id` already inserts the attachment row and
  touches the message and room (don't insert it again through storage). Each Rails save is its own
  transaction: use one `db.write` per call where Rails makes separate saves.
- (from storage) Use `campfire_storage::Storage` (disk root `storage/files`, service name
  "local"). Blocking work (vips/ffmpeg) belongs on a blocking task. What Rails does around the
  storage calls is the app's: after `insert_attachment`, touch the record (Message → also its
  room, `belongs_to :room, touch: true`); after `Storage::analyze`, touch every record from
  `blob::attachment_records` (Blob `after_update :touch_attachments`); `AnalyzeJob` after an
  attachment commit when `!blob.is_analyzed()` (`Message#process_attachment` also analyzes
  inline, then `process_representation` with the `:thumb` variation, or `process_preview` with
  `format: :webp` for videos). Disk `show` goes through `file_server::serve_file` and gets
  `Cache-Control: max-age=3600, public` (config/initializers/active_storage.rb); the disk PUT and
  direct uploads require a session (active_storage_authentication.rb). Representation redirects
  decode the variation key with `Variation::decode` — decoded values are strings, so their digest
  differs from the symbol-built one used at upload time (Rails behaves the same; don't "fix" it).
- (from assets) Serve public files first, like ActionDispatch::Static sits before the router: for
  every request call `campfire_assets::serve(&StaticRequest { method, path (raw, no query),
  accept_encoding, range, if_modified_since })` and return its status/headers/body when it is
  `Some`; otherwise continue to the app. It covers /assets/* and reference/public (404.html,
  robots.txt, `/404` -> 404.html, ...). HEAD responses already have an empty body.
## parity
- (from storage) Byte-identical variants/posters need the runtime image to ship the reference
  image's Debian trixie packages: `libvips42t64=8.16.1-1+deb13u1` and `ffmpeg=7:7.1.5-0+deb13u1`
  (verified by running `cargo test -p campfire_storage --test vectors` in `rust:1-trixie` with
  those packages). Don't install poppler-utils or mupdf-tools: the reference has neither, so
  PDFs aren't previewable. Regenerate vectors with `reference-tools/storage/run.sh`.
- (from parity/capture, for parity/bin/reference) A docker-runtime instance started with
  `parity/bin/reference up --seed default --port 4201 --time 2026-03-02T16:00:00Z --tick` has no
  Redis reachable at 127.0.0.1:6379: every `PresenceChannel` subscribe fails with
  `Redis::CannotConnectError`, so the subscription is never confirmed and the capture harness
  (correctly) reports the room as not ready. Resque workers log the same error.
- (from parity/capture, for parity/bin/reference) Captures sign in through the real form once per
  (server, user) per run; `SessionsController` allows 10 sign-ins per 3 minutes per IP in the
  Rails cache. Each instance needs its own cache/Redis so two instances (and repeated runs) don't
  share the counter.
