//! Environment configuration: the env vars the reference reads in production, plus a few
//! `CAMPFIRE_*` knobs for things Rails gets from its directory layout.
//!
//! Reference sources:
//! - `SECRET_KEY_BASE`: Rails' `secret_key_base` (required in production; `SECRET_KEY_BASE_DUMMY`
//!   makes a throwaway one, as Rails does for asset precompilation).
//! - `VAPID_PUBLIC_KEY`, `VAPID_PRIVATE_KEY`: `config/initializers/vapid.rb`. Checked at boot:
//!   when either is missing or they aren't a matching P-256 key pair, Web Push is off (logged).
//! - `VAPID_SUBJECT`: the contact push services see in the VAPID JWT's `sub` (a `mailto:` or
//!   `https:` URL). The reference hardcodes `mailto:support@37signals.com`; this defaults to
//!   `https://` and the first `TLS_DOMAIN`, or the project's URL without one.
//! - `DISABLE_SSL`: `config/environments/production.rb` (`assume_ssl`/`force_ssl` unless present).
//! - `APP_VERSION`, `GIT_REVISION`: `config/initializers/version.rb` (`X-Version`, `X-Rev`).
//! - `RAILS_ENV`: names the database file (`storage/db/<env>.sqlite3`, `config/database.yml`).
//! - `RAILS_MAX_THREADS`: `config/database.yml` pool size, used for the reader pool.
//! - `JOB_CONCURRENCY`: Resque worker count (`config/puma.rb`), used for job concurrency.
//! - `RAILS_LOG_LEVEL`: `config/environments/production.rb` log level.
//! - Thruster's (`TLS_DOMAIN`, `HTTP_PORT`, `HTTP_*_TIMEOUT`, `TARGET_PORT`, ...): read by
//!   `campfire_kit::front::FrontConfig`, which does Thruster's job in this binary.
//! - Not applicable: `REDIS_URL` and `WEB_CONCURRENCY` (no Redis, one process), `PORT` (Puma's;
//!   the app listens on Thruster's `TARGET_PORT`), and `SENTRY_DSN` and `SKIP_TELEMETRY` (the app
//!   sends no telemetry).
//! - `CAMPFIRE_FRAGMENT_CACHE_MB`: the fragment store's limit in megabytes (default 32). The
//!   reference caches fragments in Redis (`redis_cache_store`) with no `maxmemory`; this store is
//!   in the process, so it's bounded like Rails' `MemoryStore` (default `size` 32 MB), evicting the
//!   least recently used fragments. See `campfire_views::fragment_cache`.
//!
//! - Hermes fork, live voice incident reports (Gemini Live, see docs/hermes-gemini-live.md):
//!   `GEMINI_API_KEY` turns the feature on; `GEMINI_LIVE_MODEL`, `GEMINI_LIVE_VOICE_BOT`,
//!   `GEMINI_LIVE_TOKENS_PER_HOUR` and `GEMINI_LIVE_EXTRA_INSTRUCTIONS` tune it ([`GeminiLiveConfig`]).
//!   `HERMES_ASK_URL` (the Hermes bridge's `/ask/<secret>` URL, a secret: never logged) lets the
//!   interviewer ask Hermes mid-interview (`ask_hermes`), `HERMES_ASKS_PER_HOUR` caps it per user.
//!
//! Storage paths mirror `Rails.root.join("storage")`: the database under `db/`, blobs under
//! `files/` (`config/storage.yml`), backups under `backups/` (`script/admin/prepare-backup`).

use std::path::PathBuf;

use anyhow::{Context, bail};

#[derive(Debug, Clone)]
pub struct Config {
    pub secret_key_base: String,
    pub vapid_public_key: Option<String>,
    pub vapid_private_key: Option<String>,
    /// `VAPID_SUBJECT`, or a default (see the module docs).
    pub vapid_subject: String,
    /// `DISABLE_SSL` present: no `assume_ssl`, no `force_ssl`.
    pub disable_ssl: bool,
    /// `Rails.application.config.app_version`
    pub app_version: String,
    /// `Rails.application.config.git_revision`
    pub git_revision: Option<String>,
    pub environment: String,
    pub storage: StoragePaths,
    pub db_readers: usize,
    pub job_concurrency: usize,
    pub log_level: String,
    /// The fragment store's limit in bytes (`CAMPFIRE_FRAGMENT_CACHE_MB`).
    pub fragment_cache_bytes: usize,
    /// Hermes fork: lifetime of the signed `message.attachment.path` in bot webhooks
    /// (`HERMES_ATTACHMENT_URL_TTL_MINUTES`, default 60).
    pub webhook_attachment_ttl: jiff::SignedDuration,
    /// Hermes fork: live voice incident reports; `None` (routes 404, no mic button) unless
    /// `GEMINI_API_KEY` is set.
    pub gemini_live: Option<GeminiLiveConfig>,
}

/// Hermes fork: what the live voice report (`controllers::voice`) needs from the environment.
#[derive(Debug, Clone)]
pub struct GeminiLiveConfig {
    /// `GEMINI_API_KEY`: stays on the server; browsers only get single-use ephemeral tokens.
    pub api_key: ApiKey,
    /// `GEMINI_LIVE_MODEL` (default `models/gemini-3.8-live`; a bare name gets `models/`).
    pub model: String,
    /// `GEMINI_LIVE_VOICE_BOT`: the bot the report @mentions, by user id or exact name. Unset,
    /// the room's only active bot (a room with none or several can't take reports).
    pub voice_bot: Option<String>,
    /// `GEMINI_LIVE_TOKENS_PER_HOUR`: tokens one user may mint per rolling hour (default 10).
    pub tokens_per_hour: usize,
    /// `GEMINI_LIVE_EXTRA_INSTRUCTIONS`: appended to the interviewer's system instruction.
    pub extra_instructions: Option<String>,
    /// `HERMES_ASK_URL`: the Hermes bridge's `POST /ask/<secret>` (an http(s) URL; the path
    /// carries a secret, so it's kept redacted). Set, the interviewer gets the `ask_hermes` tool.
    pub hermes_ask_url: Option<ApiKey>,
    /// `HERMES_ASKS_PER_HOUR`: `ask_hermes` questions one user may ask per rolling hour (default 30).
    pub hermes_asks_per_hour: usize,
}

pub const DEFAULT_GEMINI_LIVE_MODEL: &str = "models/gemini-3.8-live";

/// A secret that never shows in `Debug` output (the config is logged on some errors).
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([REDACTED])")
    }
}

#[derive(Debug, Clone)]
pub struct StoragePaths {
    /// `storage/db/<env>.sqlite3`
    pub database: PathBuf,
    /// The `local` Disk service root, `storage/files`.
    pub files: PathBuf,
    /// `storage/backups`
    pub backups: PathBuf,
}

impl StoragePaths {
    pub fn new(root: impl Into<PathBuf>, environment: &str) -> Self {
        let root = root.into();
        Self { database: root.join("db").join(format!("{environment}.sqlite3")), files: root.join("files"), backups: root.join("backups") }
    }

    /// `config/initializers/storage_paths.rb`: `storage/{db,files}` exist after boot.
    pub fn create_dirs(&self) -> std::io::Result<()> {
        if let Some(db_dir) = self.database.parent() {
            std::fs::create_dir_all(db_dir)?;
        }
        std::fs::create_dir_all(&self.files)
    }

    /// Where `prepare-backup` writes the snapshot: `storage/backups/<database file name>`.
    pub fn backup_file(&self) -> PathBuf {
        self.backups.join(self.database.file_name().unwrap_or_else(|| "production.sqlite3".as_ref()))
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Builds the config from any variable lookup (tests pass a map).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let present = |name: &str| get(name).filter(|v| !v.trim().is_empty());

        let secret_key_base = match present("SECRET_KEY_BASE") {
            Some(secret) => secret,
            None if present("SECRET_KEY_BASE_DUMMY").is_some() => dummy_secret(),
            None => bail!("Missing `secret_key_base` for 'production' environment, set SECRET_KEY_BASE"),
        };
        let environment = present("RAILS_ENV").unwrap_or_else(|| "production".into());

        let storage_root = present("CAMPFIRE_STORAGE_PATH").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("storage"));
        let mut storage = StoragePaths::new(storage_root, &environment);
        if let Some(database) = present("CAMPFIRE_DATABASE_PATH") {
            storage.database = database.into();
        }
        if let Some(files) = present("CAMPFIRE_FILES_PATH") {
            storage.files = files.into();
        }
        if let Some(backups) = present("CAMPFIRE_BACKUPS_PATH") {
            storage.backups = backups.into();
        }

        let number = |name: &str, default: usize| -> anyhow::Result<usize> {
            match present(name) {
                Some(value) => value.trim().parse().with_context(|| format!("{name}={value:?} is not a number")),
                None => Ok(default),
            }
        };

        Ok(Self {
            secret_key_base,
            vapid_public_key: present("VAPID_PUBLIC_KEY"),
            vapid_private_key: present("VAPID_PRIVATE_KEY"),
            vapid_subject: present("VAPID_SUBJECT").unwrap_or_else(|| default_vapid_subject(present("TLS_DOMAIN"))),
            disable_ssl: present("DISABLE_SSL").is_some(),
            app_version: present("APP_VERSION").or_else(|| present("GIT_REVISION")).unwrap_or_else(|| "0".into()),
            git_revision: get("GIT_REVISION"),
            environment,
            storage,
            db_readers: number("RAILS_MAX_THREADS", 5)?.max(1),
            job_concurrency: number("JOB_CONCURRENCY", 2)?.max(1),
            log_level: present("RAILS_LOG_LEVEL").unwrap_or_else(|| "info".into()),
            fragment_cache_bytes: number("CAMPFIRE_FRAGMENT_CACHE_MB", campfire_views::fragment_cache::DEFAULT_MAX_BYTES >> 20)?
                .saturating_mul(1 << 20),
            webhook_attachment_ttl: jiff::SignedDuration::from_mins(
                i64::try_from(number("HERMES_ATTACHMENT_URL_TTL_MINUTES", 60)?.max(1)).unwrap_or(60),
            ),
            gemini_live: match present("GEMINI_API_KEY") {
                Some(key) => Some(GeminiLiveConfig {
                    api_key: ApiKey::new(key.trim()),
                    model: gemini_model_name(present("GEMINI_LIVE_MODEL").as_deref()),
                    voice_bot: present("GEMINI_LIVE_VOICE_BOT").map(|bot| bot.trim().to_string()),
                    tokens_per_hour: number("GEMINI_LIVE_TOKENS_PER_HOUR", 10)?.max(1),
                    extra_instructions: present("GEMINI_LIVE_EXTRA_INSTRUCTIONS"),
                    hermes_ask_url: match present("HERMES_ASK_URL") {
                        Some(url) => Some(hermes_ask_url(url.trim())?),
                        None => None,
                    },
                    hermes_asks_per_hour: number("HERMES_ASKS_PER_HOUR", 30)?.max(1),
                }),
                None => None,
            },
        })
    }
}

/// `models/<name>`, as `BidiGenerateContentSetup.model` wants it.
fn gemini_model_name(model: Option<&str>) -> String {
    match model.map(str::trim) {
        None => DEFAULT_GEMINI_LIVE_MODEL.to_string(),
        Some(model) if model.starts_with("models/") => model.to_string(),
        Some(model) => format!("models/{model}"),
    }
}

/// `HERMES_ASK_URL` must be an absolute http(s) URL with a host. The error never quotes it: its
/// path is a secret.
fn hermes_ask_url(url: &str) -> anyhow::Result<ApiKey> {
    let parsed = campfire_richtext::uri::parse(url).ok();
    let valid = parsed.is_some_and(|uri| uri.is_http() && uri.host.as_deref().is_some_and(|host| !host.is_empty()));
    if !valid {
        bail!("HERMES_ASK_URL must be an http:// or https:// URL (e.g. http://campfire-bridge:8645/ask/<secret>)");
    }
    Ok(ApiKey::new(url))
}

/// The install's own HTTPS URL when it has a TLS domain; the project's otherwise.
fn default_vapid_subject(tls_domains: Option<String>) -> String {
    let domain = tls_domains.as_deref().and_then(|domains| domains.split(',').map(str::trim).find(|domain| !domain.is_empty()));
    match domain {
        Some(domain) => format!("https://{domain}"),
        None => "https://github.com/basecamp/once-campfire-rust".into(),
    }
}

fn dummy_secret() -> String {
    use rand::Rng;
    let bytes: [u8; 64] = rand::rng().random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        let vars: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn requires_a_secret_key_base() {
        assert!(config(&[]).is_err());
        assert_eq!(config(&[("SECRET_KEY_BASE_DUMMY", "1")]).unwrap().secret_key_base.len(), 128);
    }

    #[test]
    fn production_defaults() {
        let config = config(&[("SECRET_KEY_BASE", "abc")]).unwrap();
        assert!(!config.disable_ssl);
        assert_eq!(config.app_version, "0");
        assert_eq!(config.git_revision, None);
        assert_eq!(config.storage.database, PathBuf::from("storage/db/production.sqlite3"));
        assert_eq!(config.storage.files, PathBuf::from("storage/files"));
        assert_eq!(config.storage.backup_file(), PathBuf::from("storage/backups/production.sqlite3"));
        assert_eq!(config.fragment_cache_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn fragment_cache_size_in_megabytes() {
        let bytes = config(&[("SECRET_KEY_BASE", "abc"), ("CAMPFIRE_FRAGMENT_CACHE_MB", "64")]).unwrap().fragment_cache_bytes;
        assert_eq!(bytes, 64 * 1024 * 1024);
        assert!(config(&[("SECRET_KEY_BASE", "abc"), ("CAMPFIRE_FRAGMENT_CACHE_MB", "lots")]).is_err());
    }

    #[test]
    fn version_falls_back_to_the_revision() {
        let config = config(&[("SECRET_KEY_BASE", "abc"), ("APP_VERSION", ""), ("GIT_REVISION", "abc123")]).unwrap();
        assert_eq!(config.app_version, "abc123");
        assert_eq!(config.git_revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn disable_ssl_is_any_non_blank_value() {
        assert!(config(&[("SECRET_KEY_BASE", "abc"), ("DISABLE_SSL", "false")]).unwrap().disable_ssl);
        assert!(!config(&[("SECRET_KEY_BASE", "abc"), ("DISABLE_SSL", " ")]).unwrap().disable_ssl);
    }

    #[test]
    fn vapid_keys_must_not_be_blank() {
        let config = config(&[("SECRET_KEY_BASE", "abc"), ("VAPID_PUBLIC_KEY", ""), ("VAPID_PRIVATE_KEY", " ")]).unwrap();
        assert_eq!((config.vapid_public_key, config.vapid_private_key), (None, None));
    }

    #[test]
    fn vapid_subject_defaults_to_the_tls_domain() {
        let subject = |vars: &[(&str, &str)]| config(&[&[("SECRET_KEY_BASE", "abc")], vars].concat()).unwrap().vapid_subject;
        assert_eq!(subject(&[("VAPID_SUBJECT", "mailto:ops@example.com"), ("TLS_DOMAIN", "chat.example.com")]), "mailto:ops@example.com");
        assert_eq!(subject(&[("TLS_DOMAIN", " , chat.example.com,other.example.com")]), "https://chat.example.com");
        assert_eq!(subject(&[("VAPID_SUBJECT", " ")]), "https://github.com/basecamp/once-campfire-rust");
    }

    #[test]
    fn gemini_live_is_off_without_a_key() {
        assert!(config(&[("SECRET_KEY_BASE", "abc"), ("GEMINI_API_KEY", " ")]).unwrap().gemini_live.is_none());
        let live = config(&[("SECRET_KEY_BASE", "abc"), ("GEMINI_API_KEY", "k")]).unwrap().gemini_live.unwrap();
        assert_eq!(live.model, "models/gemini-3.8-live");
        assert_eq!((live.voice_bot, live.tokens_per_hour, live.extra_instructions), (None, 10, None));
        assert_eq!((live.hermes_ask_url, live.hermes_asks_per_hour), (None, 30));
        assert_eq!(format!("{:?}", live.api_key), "ApiKey([REDACTED])");
    }

    #[test]
    fn gemini_live_settings() {
        let live = config(&[
            ("SECRET_KEY_BASE", "abc"),
            ("GEMINI_API_KEY", "k"),
            ("GEMINI_LIVE_MODEL", "gemini-live-x"),
            ("GEMINI_LIVE_VOICE_BOT", " Hermes "),
            ("GEMINI_LIVE_TOKENS_PER_HOUR", "3"),
        ])
        .unwrap()
        .gemini_live
        .unwrap();
        assert_eq!((live.model.as_str(), live.voice_bot.as_deref(), live.tokens_per_hour), ("models/gemini-live-x", Some("Hermes"), 3));
    }

    #[test]
    fn hermes_ask_url_is_a_redacted_http_url() {
        let with = |url: &str| {
            config(&[("SECRET_KEY_BASE", "abc"), ("GEMINI_API_KEY", "k"), ("HERMES_ASK_URL", url), ("HERMES_ASKS_PER_HOUR", "5")])
        };
        let live = with(" http://campfire-bridge:8645/ask/s3cret ").unwrap().gemini_live.unwrap();
        let url = live.hermes_ask_url.unwrap();
        assert_eq!(url.expose(), "http://campfire-bridge:8645/ask/s3cret");
        assert_eq!(format!("{url:?}"), "ApiKey([REDACTED])");
        assert_eq!(live.hermes_asks_per_hour, 5);
        for bad in ["campfire-bridge:8645/ask/s3cret", "ftp://host/ask/s3cret", "/ask/s3cret"] {
            let error = with(bad).unwrap_err().to_string();
            assert!(error.contains("HERMES_ASK_URL") && !error.contains("s3cret"), "{error}");
        }
        let blank = config(&[("SECRET_KEY_BASE", "abc"), ("GEMINI_API_KEY", "k"), ("HERMES_ASK_URL", "")]).unwrap();
        assert!(blank.gemini_live.unwrap().hermes_ask_url.is_none(), "compose's ${{VAR:+…}} gives an empty value");
        // Without Gemini there's no live session to ask from.
        assert!(config(&[("SECRET_KEY_BASE", "abc"), ("HERMES_ASK_URL", "http://x/ask/s")]).unwrap().gemini_live.is_none());
    }

    #[test]
    fn storage_overrides() {
        let config =
            config(&[("SECRET_KEY_BASE", "abc"), ("CAMPFIRE_STORAGE_PATH", "/rails/storage"), ("CAMPFIRE_FILES_PATH", "/seed/storage")])
                .unwrap();
        assert_eq!(config.storage.database, PathBuf::from("/rails/storage/db/production.sqlite3"));
        assert_eq!(config.storage.files, PathBuf::from("/seed/storage"));
    }
}
