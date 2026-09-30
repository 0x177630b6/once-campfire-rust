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
        }))
    }

    /// Where browsers open Fizzy.
    pub fn link_base(&self) -> &str {
        self.public_url.as_deref().unwrap_or(&self.fizzy_url)
    }
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
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.link_base(), "https://192.168.0.114:8444");
        assert_eq!(config.account.as_deref(), Some("897362094"));
        assert_eq!(config.poll_interval, Duration::from_secs(5));
        assert_eq!(config.incident_board, "Incidents");
    }

    #[test]
    fn rejects_bad_values_without_quoting_the_token() {
        for vars in [
            vec![("FIZZY_URL", "fizzy:3000"), ("FIZZY_TOKEN", "s3cret")],
            vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "s3cret"), ("FIZZY_PUBLIC_URL", "ftp://x")],
            vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "s3cret"), ("FIZZY_ACCOUNT", "acme")],
            vec![("FIZZY_URL", "http://fizzy"), ("FIZZY_TOKEN", "s3cret"), ("FIZZY_POLL_S", "often")],
        ] {
            let error = config(&vars).unwrap_err().to_string();
            assert!(!error.contains("s3cret"), "{error}");
        }
    }
}
