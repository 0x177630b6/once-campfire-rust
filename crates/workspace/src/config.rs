//! The workspace's environment. The feature is on only with both `FIZZY_URL` and `FIZZY_TOKEN`;
//! without them nothing of it exists (no routes, no chips, no tab bar).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `FIZZY_URL` | unset (off) | Fizzy as the server reaches it, e.g. `http://fizzy` |
//! | `FIZZY_TOKEN` | unset (off) | A Fizzy access token (`read` is enough); never logged, never sent to browsers |
//! | `FIZZY_PUBLIC_URL` | `FIZZY_URL` | Fizzy as browsers reach it, for card links, e.g. `https://192.168.0.114:8444` |
//! | `FIZZY_ACCOUNT` | the token's first account | The account slug (digits), e.g. `897362094` |
//! | `FIZZY_POLL_S` | `30` | Seconds between polls (at least 5) |
//! | `WORKSPACE_INCIDENT_BOARD` | `Incident Log` | The incident board, by name or id |
//! | `CAMPFIRE_PUBLIC_URL` | unset | Campfire as browsers reach it, for the link to the message a card was created from (unset: the message's path, as text). When its host isn't Fizzy's (`FIZZY_PUBLIC_URL`), requests on that host are "public": no Fizzy links on the pages ([`WorkspaceConfig::is_public_request`]) |
//! | `HERMES_BOT` | unset | Hermes's Campfire bot, by user id or exact name: the only bot whose proposals Campfire takes (`POST /hermes/:bot_key/workspace/proposals`). Unset: `GEMINI_LIVE_VOICE_BOT` when set, else the instance's only active bot; with several bots and neither set, proposals are refused (403) |
//! | `HERMES_FIZZY_TOKEN` | unset | Hermes's own Fizzy token (`write`): what Campfire runs for Hermes (its proposals, the undo of its comments) is written under Hermes's name, and the Hermes log learns Hermes's Fizzy user from it. Unset: those writes use `FIZZY_TOKEN` |
//!
//! The settings administrators edit in the app ([`crate::settings`]) are in
//! `<CAMPFIRE_STORAGE_PATH>/hermes/workspace.json` (`storage/hermes/workspace.json` by default).

use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_POLL_SECONDS: u64 = 30;
pub const MIN_POLL_SECONDS: u64 = 5;
pub const DEFAULT_INCIDENT_BOARD: &str = "Incident Log";

/// A secret that never shows in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    /// `FIZZY_URL` without a trailing slash.
    pub fizzy_url: String,
    /// `FIZZY_PUBLIC_URL` without a trailing slash; `None` links to `FIZZY_URL`.
    pub public_url: Option<String>,
    /// `FIZZY_ACCOUNT` (digits, no slashes); `None` uses the token's first account.
    pub account: Option<String>,
    pub token: Secret,
    pub poll_interval: Duration,
    /// `WORKSPACE_INCIDENT_BOARD`: a board name (case-insensitive) or id.
    pub incident_board: String,
    /// `<CAMPFIRE_STORAGE_PATH>/hermes/workspace.json`.
    pub settings_path: PathBuf,
    /// `CAMPFIRE_PUBLIC_URL` without a trailing slash: where links written into Fizzy point back
    /// to Campfire. `None`: those are paths, as text (the request's `Host` is never used).
    pub campfire_url: Option<String>,
    /// `HERMES_FIZZY_TOKEN`: Hermes's own Fizzy token, for what Campfire runs for Hermes (phase 2).
    pub hermes_token: Option<Secret>,
    /// `HERMES_BOT`: Hermes's Campfire bot (a user id or an exact name), the only one that may
    /// propose (phase 2).
    pub hermes_bot: Option<String>,
}

/// A configuration error. It never quotes `FIZZY_TOKEN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl WorkspaceConfig {
    /// `None` (feature off) unless both `FIZZY_URL` and `FIZZY_TOKEN` are set and non-blank.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, ConfigError> {
        let present = |name: &str| get(name).map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        let (Some(url), Some(token)) = (present("FIZZY_URL"), present("FIZZY_TOKEN")) else {
            return Ok(None);
        };
        let fizzy_url = base_url("FIZZY_URL", &url)?;
        let public_url = present("FIZZY_PUBLIC_URL").map(|url| base_url("FIZZY_PUBLIC_URL", &url)).transpose()?;
        let campfire_url = present("CAMPFIRE_PUBLIC_URL").map(|url| base_url("CAMPFIRE_PUBLIC_URL", &url)).transpose()?;
        let account = present("FIZZY_ACCOUNT").map(|slug| slug.trim_matches('/').to_string());
        if let Some(slug) = &account
            && (slug.is_empty() || !slug.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(ConfigError(format!("FIZZY_ACCOUNT must be the account's numeric slug (e.g. 897362094), got {slug:?}")));
        }
        let poll_seconds = match present("FIZZY_POLL_S") {
            Some(value) => value.parse::<u64>().map_err(|_| ConfigError(format!("FIZZY_POLL_S={value:?} is not a number of seconds")))?,
            None => DEFAULT_POLL_SECONDS,
        };
        Ok(Some(Self {
            fizzy_url,
            public_url,
            account,
            token: Secret::new(token),
            poll_interval: Duration::from_secs(poll_seconds.max(MIN_POLL_SECONDS)),
            incident_board: present("WORKSPACE_INCIDENT_BOARD").unwrap_or_else(|| DEFAULT_INCIDENT_BOARD.into()),
            settings_path: PathBuf::from(present("CAMPFIRE_STORAGE_PATH").unwrap_or_else(|| "storage".into()))
                .join("hermes")
                .join("workspace.json"),
            campfire_url,
            hermes_token: present("HERMES_FIZZY_TOKEN").map(Secret::new),
            hermes_bot: present("HERMES_BOT"),
        }))
    }

    /// `<CAMPFIRE_STORAGE_PATH>/hermes/<name>`: the workspace's files, next to the settings.
    pub fn storage_file(&self, name: &str) -> PathBuf {
        self.settings_path.parent().map(|dir| dir.join(name)).unwrap_or_else(|| PathBuf::from(name))
    }

    /// Where browsers open Fizzy.
    pub fn link_base(&self) -> &str {
        self.public_url.as_deref().unwrap_or(&self.fizzy_url)
    }

    /// Whether a request came through Campfire's public address (e.g. a Cloudflare Tunnel's
    /// hostname) rather than the LAN, where Fizzy isn't reachable: `CAMPFIRE_PUBLIC_URL` is set, its
    /// host isn't Fizzy's (on the LAN-only setup both are the LAN host: nothing is public), and one of
    /// `hosts` (the request's `Host`, each `X-Forwarded-Host`, with or without a port) is it. Any
    /// match counts, so a forged `X-Forwarded-Host` can't make a public request look local.
    pub fn is_public_request<'a>(&self, hosts: impl IntoIterator<Item = &'a str>) -> bool {
        let Some(public) = self.campfire_url.as_deref().map(url_host) else { return false };
        if public.is_empty() || url_host(self.link_base()) == public {
            return false;
        }
        hosts.into_iter().any(|host| host_only(host) == public)
    }
}

/// The host of an http(s) URL, lowercase, without its port.
fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    host_only(rest.split(['/', '?', '#']).next().unwrap_or(""))
}

/// A `Host` value without its port (`[::1]:8443` → `[::1]`), trimmed and lowercase.
fn host_only(host: &str) -> String {
    let host = host.trim();
    let host = match host.rsplit_once(':') {
        Some((name, port))
            if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && (!name.contains(':') || name.ends_with(']')) =>
        {
            name
        }
        _ => host,
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// An absolute http(s) URL with a host, without its trailing slashes.
fn base_url(name: &str, url: &str) -> Result<String, ConfigError> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"));
    let host = rest.map(|rest| rest.split(['/', '?', '#']).next().unwrap_or(""));
    if host.is_none_or(|host| host.is_empty() || host.contains('@')) || url.contains(['?', '#', ' ']) {
        return Err(ConfigError(format!("{name} must be an http:// or https:// URL such as http://fizzy")));
    }
    Ok(url.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Option<WorkspaceConfig>, ConfigError> {
        let vars: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        WorkspaceConfig::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn off_without_url_and_token() {
        assert_eq!(config(&[]).unwrap(), None);
        assert_eq!(config(&[("FIZZY_URL", "http://fizzy")]).unwrap(), None);
        assert_eq!(config(&[("FIZZY_TOKEN", "t")]).unwrap(), None);
        assert_eq!(config(&[("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", " ")]).unwrap(), None);
    }

    #[test]
    fn defaults() {
        let config = config(&[("FIZZY_URL", " http://fizzy/ "), ("FIZZY_TOKEN", "s3cret")]).unwrap().unwrap();
        assert_eq!(config.fizzy_url, "http://fizzy");
        assert_eq!((config.public_url.as_deref(), config.account.as_deref()), (None, None));
        assert_eq!(config.poll_interval, Duration::from_secs(30));
        assert_eq!(config.incident_board, "Incident Log");
        assert_eq!(config.link_base(), "http://fizzy");
        assert_eq!(config.settings_path, PathBuf::from("storage/hermes/workspace.json"));
        assert_eq!(config.campfire_url, None);
        assert_eq!(config.hermes_token, None);
        assert_eq!(config.storage_file("actions.jsonl"), PathBuf::from("storage/hermes/actions.jsonl"));
        assert!(!format!("{config:?}").contains("s3cret"));
    }

    #[test]
    fn settings() {
        let config = config(&[
            ("FIZZY_URL", "http://fizzy"),
            ("FIZZY_TOKEN", "t"),
            ("FIZZY_PUBLIC_URL", "https://192.168.0.114:8444/"),
            ("FIZZY_ACCOUNT", "/897362094"),
            ("FIZZY_POLL_S", "2"),
            ("WORKSPACE_INCIDENT_BOARD", "Incidents"),
            ("CAMPFIRE_STORAGE_PATH", "/rails/storage"),
            ("CAMPFIRE_PUBLIC_URL", "https://192.168.0.114:8443/"),
            ("HERMES_FIZZY_TOKEN", " h3rmes "),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.hermes_token.as_ref().map(Secret::expose), Some("h3rmes"));
        assert!(!format!("{config:?}").contains("h3rmes"));
        assert_eq!(config.campfire_url.as_deref(), Some("https://192.168.0.114:8443"));
        assert_eq!(config.settings_path, PathBuf::from("/rails/storage/hermes/workspace.json"));
        assert_eq!(config.link_base(), "https://192.168.0.114:8444");
        assert_eq!(config.account.as_deref(), Some("897362094"));
        assert_eq!(config.poll_interval, Duration::from_secs(5));
        assert_eq!(config.incident_board, "Incidents");
    }

    #[test]
    fn public_requests_are_those_on_campfires_public_host() {
        let with = |campfire: Option<&str>| {
            let mut vars = vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "t"), ("FIZZY_PUBLIC_URL", "https://192.168.0.114:8444")];
            vars.extend(campfire.map(|url| ("CAMPFIRE_PUBLIC_URL", url)));
            config(&vars).unwrap().unwrap()
        };
        // The LAN-only setup: CAMPFIRE_PUBLIC_URL unset, or the LAN address like Fizzy's.
        assert!(!with(None).is_public_request(["chat.example.com"]));
        assert!(!with(Some("https://192.168.0.114:8443")).is_public_request(["192.168.0.114:8443"]));
        // Published through a tunnel: its host is public, the LAN address isn't.
        let tunnel = with(Some("https://Chat.Example.com/"));
        assert!(tunnel.is_public_request(["chat.example.com"]));
        assert!(tunnel.is_public_request(["CHAT.example.com:443"]));
        assert!(!tunnel.is_public_request(["192.168.0.114:8443"]));
        assert!(!tunnel.is_public_request(["chat.example.com.evil", "example.com"]));
        assert!(!tunnel.is_public_request([]));
        // A forged X-Forwarded-Host next to the public Host doesn't make it local.
        assert!(tunnel.is_public_request(["chat.example.com", "192.168.0.114:8443"]));
        assert_eq!((host_only("[::1]:8443"), host_only("[::1]"), host_only("Host.")), ("[::1]".into(), "[::1]".into(), "host".into()));
    }

    #[test]
    fn rejects_bad_values_without_quoting_the_token() {
        for vars in [
            vec![("FIZZY_URL", "fizzy:3000"), ("FIZZY_TOKEN", "s3cret")],
            vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "s3cret"), ("FIZZY_PUBLIC_URL", "ftp://x")],
            vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "s3cret"), ("FIZZY_ACCOUNT", "acme")],
            vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "s3cret"), ("FIZZY_POLL_S", "often")],
            vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "s3cret"), ("CAMPFIRE_PUBLIC_URL", "chat.example")],
        ] {
            let error = config(&vars).unwrap_err().to_string();
            assert!(!error.contains("s3cret"), "{error}");
        }
    }
}
