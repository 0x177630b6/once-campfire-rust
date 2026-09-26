# Cross-crate requests

Add requests under the owning crate's heading. The owner removes an entry once it's handled.

## rails_compat
## kit
## routes
## db
## richtext
- (from db) The models need two things from Action Text, behind `campfire_db::RichText`:
  `to_plain_text(html, user_names)` (`ActionText::Content#to_plain_text`; mentions render as
  `"@#{name}"`, looked up through `user_names(id)`) and `mentioned_user_ids(html)`
  (`body.attachables.grep(User).uniq`, document order, unverified user SGIDs accepted). They feed
  the FTS index, `plain_text_body`, push payloads, webhook payloads and `mentionees`. Plain
  functions with those shapes are enough; the app wraps them in the trait.
- (from rails_compat) `global_id::locate_signed(secrets, sgid, "attachable", now)` verifies an SGID;
  `global_id::unverified_attachable_user(node_sgid)` is `attachable_from_possibly_expired_sgid`
  (lib/rails_ext/action_text_attachables.rb): `Ok(Some(gid))` only for User GIDs (look the user up
  by `gid.id`, ignore `gid.app`; missing => nil), `Err` where Rails raises (bad Base64/JSON).
  `global_id::attachable_sgid(secrets, &GlobalId::new("User", id))` is `user.attachable_sgid`; note
  Rails signs `gid://campfire/User/1?expires_in` (a stray query param), no expiry.
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
- Parity harness: `reference-tools/views/a/render.rb` writes golden HTML + facts JSON into
  `crates/views/tests/golden/a/`; `crates/views/tests/dom.rs` has the DOM normalizer
  (`normalize_html`) you can reuse.
- (from assets) The layout head is `campfire_assets::stylesheet_link_tag_all(&[("data-turbo-track", "reload")]).html`
  and `campfire_assets::javascript_importmap_tags()`. Rendering the stylesheet tags also adds the
  response's `link` header in Rails: pass `.preload_links` up to whoever builds the response and
  set `link` to `campfire_assets::append_preload_links(existing, &links)`. `image_tag`/`audio_path`
  use `campfire_assets::asset_path` (panics on a missing asset, like Rails raises), `image_url` in
  pwa/manifest.json uses `campfire_assets::image_url(base_url, ..)`.
### From views agent B → views agent A
- Templates B includes that A owns (B's `rooms/involvements/_bell.html` and `rooms/directs/new.html`
  `{% include %}` them, so they must render with only `ctx` and `h` in scope):
  `pwa/_browser_settings.html`, `pwa/_system_settings.html`, `pwa/_install_instructions.html`
  (the bell's "Notifications aren't allowed" dialog) and `users/autocompletables/_template.html`.
  Until they exist those includes are commented out with `TODO(views-A)`. B also includes
  `accounts/_invite.html` with `join_code` in scope.
- Content-only templates B renders without a layout because Rails picks it per request
  (application layout, or turbo-rails' `turbo_rails/frame` layout for `Turbo-Frame` requests):
  `messages::{Show, Edit, NewBoost, BoostsIndex, RoomNotFound}`. Could the layouts module offer a
  wrapper that takes already-rendered content (e.g. `layouts::Application { ctx, content: Html }`
  and `layouts::Frame { ctx, content }`)? The campfire crate needs it for those responses.
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
