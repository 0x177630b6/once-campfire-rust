//! Sky push-to-talk, the pure part (docs/hermes-gemini-live.md, "Sky push-to-talk"; plan:
//! Hermes-self `docs/ui-redesign/10-push-to-talk-plan.md`). Batch S1 lays the plumbing only:
//!
//! - [`SkyConfig`]: the `SKY_*` environment, read by [`WorkspaceConfig::from_lookup`](crate::WorkspaceConfig::from_lookup)
//!   (so no new seam in Campfire's own config). `SKY_PTT` is `off` by default: no route, no button.
//! - [`Sky`]: Sky's own limits, apart from the voice page's (which keeps `GEMINI_LIVE_TOKENS_PER_HOUR`
//!   and its 30-minute tokens): tokens per person per rolling hour (a reconnection of an open session
//!   counts a quarter), presses per person per day, Hermes questions per person per rolling hour, and
//!   the organization's estimated month budget. The day and month counters persist in
//!   `<CAMPFIRE_STORAGE_PATH>/hermes/sky-usage.json` (atomic writes, 90 days kept), so a restart
//!   doesn't reset the budget; the rolling hourly windows are in memory, like the voice page's.
//!
//! Days and months are the house's (the handover's time zone, passed in by the caller). Nothing
//! here stores words, audio or tokens: counters only (decision O3).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ToSpan};
use serde::{Deserialize, Serialize};

use crate::config::ConfigError;
use crate::store;

/// A Sky token's `expireTime`: 10 minutes (the voice page keeps 30). Bounds what a misused token
/// can cost; a warm session is far shorter.
pub const TOKEN_LIFETIME: SignedDuration = SignedDuration::from_mins(10);
/// A Sky token's `newSessionExpireTime`, as the voice page's.
pub const NEW_SESSION_WINDOW: SignedDuration = SignedDuration::from_mins(1);

pub const DEFAULT_TOKENS_PER_HOUR: u32 = 30;
pub const DEFAULT_PRESSES_PER_DAY: u32 = 150;
pub const DEFAULT_ASKS_PER_HOUR: u32 = 30;
pub const DEFAULT_MONTHLY_BUDGET_USD: u32 = 100;
pub const DEFAULT_WARM_SECONDS: u64 = 120;
pub const MAX_WARM_SECONDS: u64 = 600;

/// The usage file, next to `workspace.json`.
pub const USAGE_FILE: &str = "sky-usage.json";
/// Days of per-person counters kept (decision O3).
pub const RETENTION_DAYS: i64 = 90;
/// Months of organization totals kept.
pub const RETENTION_MONTHS: i64 = 13;

/// The rolling window of the hourly limits.
const HOUR: SignedDuration = SignedDuration::from_hours(1);
/// Token weights, in quarters: a new session costs a whole token, a reconnection of an open one
/// (`goAway`, a resumption handle present) a quarter.
const FULL_TOKEN_UNITS: u32 = 4;
const RECONNECT_UNITS: u32 = 1;
/// The most one token can add to the estimated cost: 10 minutes of audio in ($0.005/min) and out
/// ($0.018/min). The page reports durations (untrusted); clamping each report to this keeps a
/// lying client within one token's worth per token minted (plan §5.1).
pub const TOKEN_COST_CEILING_MICRO_USD: u64 = 10 * (5_000 + 18_000);

// --- Configuration ----------------------------------------------------------------------------

/// `SKY_PTT`: who gets the push-to-talk button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Nobody (default): every `/sky/*` route 404, nothing rendered.
    #[default]
    Off,
    /// The phase 0 spike: administrators only, on the bare test page.
    Spike,
    /// Administrators only.
    Admins,
    /// The pilot: the ids in `SKY_PTT_USERS`, and administrators.
    Users,
    /// Everyone signed in (bots never).
    On,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "spike" => Some(Self::Spike),
            "admins" => Some(Self::Admins),
            "users" => Some(Self::Users),
            "on" => Some(Self::On),
            _ => None,
        }
    }
}

/// The `SKY_*` environment.
///
/// | Variable | Default | Meaning |
/// |---|---|---|
/// | `SKY_PTT` | `off` | `off` \| `spike` \| `admins` \| `users` \| `on` ([`Mode`]) |
/// | `SKY_PTT_USERS` | empty | Pilot user ids for `users`, comma-separated (`1,5,9`) |
/// | `SKY_TOKENS_PER_HOUR` | `30` | Sky tokens per person per rolling hour (a reconnection counts ¼) |
/// | `SKY_PRESSES_PER_DAY` | `150` | Presses per person per house day |
/// | `SKY_ASKS_PER_HOUR` | `30` | Questions to Hermes per person per rolling hour |
/// | `SKY_MONTHLY_BUDGET_USD` | `100` | The organization's estimated month budget; reached, no more tokens until the 1st |
/// | `SKY_WARM_SECONDS` | `120` | Idle seconds before the page closes a warm session (at most 600) |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkyConfig {
    pub mode: Mode,
    pub users: BTreeSet<i64>,
    pub tokens_per_hour: u32,
    pub presses_per_day: u32,
    pub asks_per_hour: u32,
    pub monthly_budget_usd: u32,
    pub warm_seconds: u64,
}

impl Default for SkyConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            users: BTreeSet::new(),
            tokens_per_hour: DEFAULT_TOKENS_PER_HOUR,
            presses_per_day: DEFAULT_PRESSES_PER_DAY,
            asks_per_hour: DEFAULT_ASKS_PER_HOUR,
            monthly_budget_usd: DEFAULT_MONTHLY_BUDGET_USD,
            warm_seconds: DEFAULT_WARM_SECONDS,
        }
    }
}

impl SkyConfig {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let present = |name: &str| get(name).map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        let number = |name: &str, default: u64| -> Result<u64, ConfigError> {
            match present(name) {
                Some(value) => value.parse::<u64>().map_err(|_| ConfigError(format!("{name}={value:?} is not a whole number"))),
                None => Ok(default),
            }
        };
        let cap = |name: &str, default: u32| -> Result<u32, ConfigError> {
            Ok(u32::try_from(number(name, u64::from(default))?).unwrap_or(u32::MAX).max(1))
        };
        let mode = match present("SKY_PTT") {
            Some(value) => {
                Mode::parse(&value).ok_or_else(|| ConfigError(format!("SKY_PTT={value:?} must be off, spike, admins, users or on")))?
            }
            None => Mode::Off,
        };
        let mut users = BTreeSet::new();
        for id in present("SKY_PTT_USERS").unwrap_or_default().split(',').map(str::trim).filter(|id| !id.is_empty()) {
            let id = id.parse::<i64>().map_err(|_| ConfigError(format!("SKY_PTT_USERS: {id:?} is not a user id")))?;
            users.insert(id);
        }
        Ok(Self {
            mode,
            users,
            tokens_per_hour: cap("SKY_TOKENS_PER_HOUR", DEFAULT_TOKENS_PER_HOUR)?,
            presses_per_day: cap("SKY_PRESSES_PER_DAY", DEFAULT_PRESSES_PER_DAY)?,
            asks_per_hour: cap("SKY_ASKS_PER_HOUR", DEFAULT_ASKS_PER_HOUR)?,
            monthly_budget_usd: cap("SKY_MONTHLY_BUDGET_USD", DEFAULT_MONTHLY_BUDGET_USD)?,
            warm_seconds: number("SKY_WARM_SECONDS", DEFAULT_WARM_SECONDS)?.clamp(1, MAX_WARM_SECONDS),
        })
    }

    /// Whether a signed-in person (never a bot: the caller checks) gets push-to-talk.
    pub fn allows(&self, user_id: i64, administrator: bool) -> bool {
        match self.mode {
            Mode::Off => false,
            Mode::Spike | Mode::Admins => administrator,
            Mode::Users => administrator || self.users.contains(&user_id),
            Mode::On => true,
        }
    }
}

// --- Rolling limits ---------------------------------------------------------------------------

/// At most `capacity` units per user in any rolling `window`, in memory (one process). The voice
/// page's `RateLimiter` algorithm, with weights.
#[derive(Debug)]
pub struct RollingLimiter {
    capacity: u32,
    window: SignedDuration,
    entries: Mutex<HashMap<i64, Vec<(Timestamp, u32)>>>,
}

impl RollingLimiter {
    pub fn new(capacity: u32, window: SignedDuration) -> Self {
        Self { capacity, window, entries: Mutex::new(HashMap::new()) }
    }

    /// Spends `units` for `user_id`; false (nothing spent) when that would pass the capacity.
    pub fn allow(&self, user_id: i64, units: u32, now: Timestamp) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let since = now - self.window;
        entries.retain(|_, spent| {
            spent.retain(|(at, _)| *at > since);
            !spent.is_empty()
        });
        let spent = entries.entry(user_id).or_default();
        let used: u32 = spent.iter().map(|(_, units)| units).sum();
        if used.saturating_add(units) > self.capacity {
            return false;
        }
        spent.push((now, units));
        true
    }
}

// --- Usage ------------------------------------------------------------------------------------

/// One person's counters for one house day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayUsage {
    pub presses: u32,
    /// New sessions' tokens.
    pub tokens: u32,
    /// Reconnections' tokens.
    pub reconnects: u32,
    pub asks: u32,
    pub confirms: u32,
    /// Requests refused by a limit or the budget.
    pub refusals: u32,
    pub errors: u32,
    /// Estimated cost, in millionths of a dollar.
    pub cost_micro_usd: u64,
}

/// The organization's totals for one house month.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonthUsage {
    pub presses: u32,
    pub tokens: u32,
    pub reconnects: u32,
    pub asks: u32,
    pub cost_micro_usd: u64,
}

/// `sky-usage.json`: `days["2026-10-01"]["12"]` and `months["2026-10"]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageFile {
    pub version: u32,
    pub days: BTreeMap<String, BTreeMap<i64, DayUsage>>,
    pub months: BTreeMap<String, MonthUsage>,
}

/// Why a Sky request is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A rolling hourly limit (`429`).
    RateLimited,
    /// The person's presses for today (`429`).
    DailyCap,
    /// The month budget is spent: no more tokens until the 1st (`budget_paused`).
    BudgetPaused,
}

/// A token for a new session, or for reconnecting an open one (counts a quarter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    New,
    Reconnect,
}

/// Counted outcomes besides the gated requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Confirm,
    Error,
}

/// The month budget, now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub spent_micro_usd: u64,
    pub limit_micro_usd: u64,
}

impl Budget {
    /// Spent, in whole percent of the limit.
    pub fn percent(&self) -> u64 {
        self.spent_micro_usd.saturating_mul(100) / self.limit_micro_usd.max(1)
    }

    pub fn paused(&self) -> bool {
        self.spent_micro_usd >= self.limit_micro_usd
    }
}

/// Sky's limits and usage counters (one per process, next to the workspace).
#[derive(Debug)]
pub struct Sky {
    config: SkyConfig,
    path: PathBuf,
    tokens: RollingLimiter,
    asks: RollingLimiter,
    usage: Mutex<Usage>,
}

#[derive(Debug, Default)]
struct Usage {
    file: UsageFile,
    /// Changed since the last [`Sky::save`].
    dirty: bool,
}

impl Sky {
    /// Sky with the usage saved at `path` (`WorkspaceConfig::storage_file(USAGE_FILE)`). A missing
    /// file starts empty; one that can't be read or decoded also starts empty, and the error is
    /// returned for the caller to log (it is replaced on the next save).
    pub fn open(config: SkyConfig, path: PathBuf) -> (Self, Option<String>) {
        let (file, error) = match load(&path) {
            Ok(file) => (file, None),
            Err(error) => (UsageFile::default(), Some(error)),
        };
        let tokens = RollingLimiter::new(config.tokens_per_hour.saturating_mul(FULL_TOKEN_UNITS), HOUR);
        let asks = RollingLimiter::new(config.asks_per_hour, HOUR);
        (Self { config, path, tokens, asks, usage: Mutex::new(Usage { file, dirty: false }) }, error)
    }

    pub fn config(&self) -> &SkyConfig {
        &self.config
    }

    /// A Gemini token for `user_id`: refused once the month budget is spent, or past the hourly
    /// limit; counted otherwise.
    pub fn allow_token(&self, user_id: i64, kind: TokenKind, now: Timestamp, zone: &TimeZone) -> Result<(), Refusal> {
        if self.budget(now, zone).paused() {
            self.refused(user_id, now, zone);
            return Err(Refusal::BudgetPaused);
        }
        let units = match kind {
            TokenKind::New => FULL_TOKEN_UNITS,
            TokenKind::Reconnect => RECONNECT_UNITS,
        };
        if !self.tokens.allow(user_id, units, now) {
            self.refused(user_id, now, zone);
            return Err(Refusal::RateLimited);
        }
        self.count(user_id, now, zone, |day, month| match kind {
            TokenKind::New => {
                day.tokens += 1;
                month.tokens += 1;
            }
            TokenKind::Reconnect => {
                day.reconnects += 1;
                month.reconnects += 1;
            }
        });
        Ok(())
    }

    /// A press (one hold of the button): at most `SKY_PRESSES_PER_DAY` per person per house day.
    pub fn allow_press(&self, user_id: i64, now: Timestamp, zone: &TimeZone) -> Result<(), Refusal> {
        let (day_key, _) = keys(now, zone);
        let mut usage = self.lock();
        let presses = usage.file.days.get(&day_key).and_then(|day| day.get(&user_id)).map_or(0, |day| day.presses);
        if presses >= self.config.presses_per_day {
            count(&mut usage, user_id, now, zone, |day, _| day.refusals += 1);
            return Err(Refusal::DailyCap);
        }
        count(&mut usage, user_id, now, zone, |day, month| {
            day.presses += 1;
            month.presses += 1;
        });
        Ok(())
    }

    /// A question to Hermes: at most `SKY_ASKS_PER_HOUR` per person per rolling hour.
    pub fn allow_ask(&self, user_id: i64, now: Timestamp, zone: &TimeZone) -> Result<(), Refusal> {
        if !self.asks.allow(user_id, 1, now) {
            self.refused(user_id, now, zone);
            return Err(Refusal::RateLimited);
        }
        self.count(user_id, now, zone, |day, month| {
            day.asks += 1;
            month.asks += 1;
        });
        Ok(())
    }

    /// Adds one exchange's estimated cost (reported by the page), clamped to
    /// [`TOKEN_COST_CEILING_MICRO_USD`].
    pub fn record_cost(&self, user_id: i64, micro_usd: u64, now: Timestamp, zone: &TimeZone) {
        let cost = micro_usd.min(TOKEN_COST_CEILING_MICRO_USD);
        self.count(user_id, now, zone, |day, month| {
            day.cost_micro_usd = day.cost_micro_usd.saturating_add(cost);
            month.cost_micro_usd = month.cost_micro_usd.saturating_add(cost);
        });
    }

    pub fn record(&self, user_id: i64, outcome: Outcome, now: Timestamp, zone: &TimeZone) {
        self.count(user_id, now, zone, |day, _| match outcome {
            Outcome::Confirm => day.confirms += 1,
            Outcome::Error => day.errors += 1,
        });
    }

    /// This house month's estimated spend against `SKY_MONTHLY_BUDGET_USD`.
    pub fn budget(&self, now: Timestamp, zone: &TimeZone) -> Budget {
        let (_, month_key) = keys(now, zone);
        let spent = self.lock().file.months.get(&month_key).map_or(0, |month| month.cost_micro_usd);
        Budget { spent_micro_usd: spent, limit_micro_usd: u64::from(self.config.monthly_budget_usd) * 1_000_000 }
    }

    /// `user_id`'s counters for today (house day).
    pub fn today(&self, user_id: i64, now: Timestamp, zone: &TimeZone) -> DayUsage {
        let (day_key, _) = keys(now, zone);
        self.lock().file.days.get(&day_key).and_then(|day| day.get(&user_id)).copied().unwrap_or_default()
    }

    /// A copy of everything counted (the admin panel, the health check's reader, tests).
    pub fn usage(&self) -> UsageFile {
        self.lock().file.clone()
    }

    /// Writes the counters if they changed since the last save (atomically). Blocking file I/O:
    /// call it off the async runtime's worker threads (`spawn_blocking`). On failure the counters
    /// stay marked as changed, so the next save retries.
    pub fn save(&self) -> std::io::Result<bool> {
        let file = {
            let mut usage = self.lock();
            if !usage.dirty {
                return Ok(false);
            }
            usage.dirty = false;
            usage.file.clone()
        };
        store::write_json_atomically(&self.path, &file).inspect_err(|_| self.lock().dirty = true)?;
        Ok(true)
    }

    fn refused(&self, user_id: i64, now: Timestamp, zone: &TimeZone) {
        self.count(user_id, now, zone, |day, _| day.refusals += 1);
    }

    fn count(&self, user_id: i64, now: Timestamp, zone: &TimeZone, change: impl FnOnce(&mut DayUsage, &mut MonthUsage)) {
        count(&mut self.lock(), user_id, now, zone, change);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Usage> {
        self.usage.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Applies `change` to `user_id`'s day and the month of `now`, then drops what's past retention.
fn count(usage: &mut Usage, user_id: i64, now: Timestamp, zone: &TimeZone, change: impl FnOnce(&mut DayUsage, &mut MonthUsage)) {
    let (day_key, month_key) = keys(now, zone);
    let file = &mut usage.file;
    file.version = 1;
    let day = file.days.entry(day_key).or_default().entry(user_id).or_default();
    let month = file.months.entry(month_key).or_default();
    change(day, month);
    prune(file, now, zone);
    usage.dirty = true;
}

/// `("2026-10-01", "2026-10")`: the house day and month of `now`.
fn keys(now: Timestamp, zone: &TimeZone) -> (String, String) {
    let date = now.to_zoned(zone.clone()).date();
    (date.to_string(), month_key(date))
}

fn month_key(date: Date) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

/// Drops days older than [`RETENTION_DAYS`] and months older than [`RETENTION_MONTHS`] (keys are
/// ISO dates, so they sort chronologically).
fn prune(file: &mut UsageFile, now: Timestamp, zone: &TimeZone) {
    let today = now.to_zoned(zone.clone()).date();
    if let Ok(oldest) = today.checked_sub(RETENTION_DAYS.days()) {
        let oldest = oldest.to_string();
        file.days.retain(|day, _| *day >= oldest);
    }
    if let Ok(oldest) = today.first_of_month().checked_sub(RETENTION_MONTHS.months()) {
        let oldest = month_key(oldest);
        file.months.retain(|month, _| *month >= oldest);
    }
}

fn load(path: &Path) -> Result<UsageFile, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| format!("{} is not valid: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(UsageFile::default()),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<SkyConfig, ConfigError> {
        let vars: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        SkyConfig::from_lookup(|name| vars.get(name).cloned())
    }

    fn paris() -> TimeZone {
        crate::shifts::time_zone("Europe/Paris").unwrap()
    }

    fn at(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    fn sky(config: SkyConfig) -> (Sky, PathBuf) {
        let path = store::scratch_dir("sky").join(USAGE_FILE);
        let (sky, error) = Sky::open(config, path.clone());
        assert_eq!(error, None);
        (sky, path)
    }

    #[test]
    fn defaults_are_off_with_the_plans_caps() {
        let config = config(&[]).unwrap();
        assert_eq!(config, SkyConfig::default());
        assert_eq!(config.mode, Mode::Off);
        assert_eq!((config.tokens_per_hour, config.presses_per_day, config.asks_per_hour), (30, 150, 30));
        assert_eq!((config.monthly_budget_usd, config.warm_seconds), (100, 120));
        assert!(!config.allows(1, true), "off is off, even for administrators");
        assert_eq!(TOKEN_LIFETIME, SignedDuration::from_mins(10));
    }

    #[test]
    fn reads_the_environment() {
        let config = config(&[
            ("SKY_PTT", " Users "),
            ("SKY_PTT_USERS", "1, 5,9,"),
            ("SKY_TOKENS_PER_HOUR", "12"),
            ("SKY_PRESSES_PER_DAY", "0"),
            ("SKY_ASKS_PER_HOUR", "7"),
            ("SKY_MONTHLY_BUDGET_USD", "250"),
            ("SKY_WARM_SECONDS", "9000"),
        ])
        .unwrap();
        assert_eq!(config.mode, Mode::Users);
        assert_eq!(config.users, BTreeSet::from([1, 5, 9]));
        assert_eq!((config.tokens_per_hour, config.presses_per_day, config.asks_per_hour), (12, 1, 7), "caps are at least 1");
        assert_eq!((config.monthly_budget_usd, config.warm_seconds), (250, MAX_WARM_SECONDS));
        assert!(config.allows(5, false) && config.allows(2, true) && !config.allows(2, false));

        for (mode, admin, user) in [("spike", true, false), ("admins", true, false), ("on", true, true)] {
            let config = self::config(&[("SKY_PTT", mode)]).unwrap();
            assert_eq!((config.allows(1, true), config.allows(1, false)), (admin, user), "{mode}");
        }
        for bad in [("SKY_PTT", "yes"), ("SKY_PTT_USERS", "1,bob"), ("SKY_TOKENS_PER_HOUR", "many"), ("SKY_WARM_SECONDS", "-1")] {
            assert!(self::config(&[bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_rolling_window_rolls_over_per_user() {
        let limiter = RollingLimiter::new(2, HOUR);
        let t0 = at("2026-10-01T12:00:00Z");
        assert!(limiter.allow(1, 1, t0));
        assert!(limiter.allow(1, 1, t0 + SignedDuration::from_mins(10)));
        assert!(!limiter.allow(1, 1, t0 + SignedDuration::from_mins(20)));
        assert!(limiter.allow(2, 1, t0 + SignedDuration::from_mins(20)), "per user");
        assert!(limiter.allow(1, 1, t0 + SignedDuration::from_mins(61)), "the first one has aged out");
        assert!(!limiter.allow(1, 1, t0 + SignedDuration::from_mins(62)));
        assert!(!RollingLimiter::new(3, HOUR).allow(1, 4, t0), "a request heavier than the capacity");
    }

    #[test]
    fn tokens_are_limited_per_hour_and_reconnections_count_a_quarter() {
        let (sky, _) = sky(SkyConfig { tokens_per_hour: 2, ..SkyConfig::default() });
        let zone = paris();
        let t0 = at("2026-10-01T12:00:00Z");
        assert_eq!(sky.allow_token(1, TokenKind::New, t0, &zone), Ok(()));
        for _ in 0..4 {
            assert_eq!(sky.allow_token(1, TokenKind::Reconnect, t0, &zone), Ok(()));
        }
        assert_eq!(sky.allow_token(1, TokenKind::Reconnect, t0, &zone), Err(Refusal::RateLimited), "1 + 4 × ¼ = 2");
        assert_eq!(sky.allow_token(2, TokenKind::New, t0, &zone), Ok(()), "per user");
        assert_eq!(sky.allow_token(1, TokenKind::New, t0 + SignedDuration::from_mins(61), &zone), Ok(()));
        let today = sky.today(1, t0, &zone);
        assert_eq!((today.tokens, today.reconnects, today.refusals), (2, 4, 1));
        assert_eq!(sky.usage().months["2026-10"].tokens, 3);
    }

    #[test]
    fn presses_are_capped_per_house_day() {
        let (sky, _) = sky(SkyConfig { presses_per_day: 2, ..SkyConfig::default() });
        let zone = paris();
        // 23:30 in Paris on 1 October is 21:30 UTC; 00:10 on the 2nd is 22:10 UTC.
        let evening = at("2026-10-01T21:30:00Z");
        assert_eq!(sky.allow_press(1, evening, &zone), Ok(()));
        assert_eq!(sky.allow_press(1, evening, &zone), Ok(()));
        assert_eq!(sky.allow_press(1, evening, &zone), Err(Refusal::DailyCap));
        assert_eq!(sky.allow_press(2, evening, &zone), Ok(()), "per user");
        let after_midnight = at("2026-10-01T22:10:00Z");
        assert_eq!(sky.allow_press(1, after_midnight, &zone), Ok(()), "a new day in Paris, still the 1st in UTC");
        let usage = sky.usage();
        assert_eq!(usage.days["2026-10-01"][&1].presses, 2);
        assert_eq!(usage.days["2026-10-02"][&1].presses, 1);
    }

    #[test]
    fn asks_are_limited_per_hour() {
        let (sky, _) = sky(SkyConfig { asks_per_hour: 1, ..SkyConfig::default() });
        let zone = paris();
        let t0 = at("2026-10-01T12:00:00Z");
        assert_eq!(sky.allow_ask(1, t0, &zone), Ok(()));
        assert_eq!(sky.allow_ask(1, t0, &zone), Err(Refusal::RateLimited));
        assert_eq!(sky.allow_ask(1, t0 + SignedDuration::from_mins(61), &zone), Ok(()));
        assert_eq!(sky.today(1, t0, &zone).asks, 2);
    }

    #[test]
    fn the_month_budget_pauses_tokens_until_the_first() {
        let (sky, _) = sky(SkyConfig { monthly_budget_usd: 1, tokens_per_hour: 1000, ..SkyConfig::default() });
        let zone = paris();
        let t0 = at("2026-10-31T12:00:00Z");
        // A lying page can't add more than one token's ceiling per report.
        sky.record_cost(1, u64::MAX, t0, &zone);
        assert_eq!(sky.budget(t0, &zone).spent_micro_usd, TOKEN_COST_CEILING_MICRO_USD);
        for _ in 0..4 {
            sky.record_cost(1, 200_000, t0, &zone);
        }
        let budget = sky.budget(t0, &zone);
        assert_eq!((budget.spent_micro_usd, budget.limit_micro_usd, budget.percent()), (1_030_000, 1_000_000, 103));
        assert!(budget.paused());
        assert_eq!(sky.allow_token(2, TokenKind::New, t0, &zone), Err(Refusal::BudgetPaused), "for everyone");
        // 1 November, 00:30 in Paris (23:30 UTC on the 31st): a new month.
        let november = at("2026-10-31T23:30:00Z");
        assert!(!sky.budget(november, &zone).paused());
        assert_eq!(sky.allow_token(2, TokenKind::New, november, &zone), Ok(()));
        assert_eq!(Budget { spent_micro_usd: 500_000, limit_micro_usd: 1_000_000 }.percent(), 50);
    }

    #[test]
    fn counters_survive_a_restart_and_old_days_are_dropped() {
        let zone = paris();
        let (sky, path) = sky(SkyConfig::default());
        let old = at("2026-06-01T12:00:00Z");
        sky.allow_press(1, old, &zone).unwrap();
        assert!(sky.save().unwrap());
        assert!(!sky.save().unwrap(), "nothing changed");
        let now = at("2026-10-01T12:00:00Z");
        sky.allow_press(1, now, &zone).unwrap();
        sky.allow_token(1, TokenKind::New, now, &zone).unwrap();
        sky.record(1, Outcome::Confirm, now, &zone);
        sky.record(1, Outcome::Error, now, &zone);
        sky.record_cost(1, 5_000, now, &zone);
        assert!(sky.save().unwrap());

        let (reopened, error) = Sky::open(SkyConfig::default(), path.clone());
        assert_eq!(error, None);
        let usage = reopened.usage();
        assert_eq!(usage, sky.usage());
        assert!(!usage.days.contains_key("2026-06-01"), "older than 90 days");
        assert!(usage.months.contains_key("2026-06"), "month totals are kept longer");
        let today = reopened.today(1, now, &zone);
        assert_eq!(today, DayUsage { presses: 1, tokens: 1, confirms: 1, errors: 1, cost_micro_usd: 5_000, ..DayUsage::default() });
        assert_eq!(reopened.budget(now, &zone).spent_micro_usd, 5_000);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"2026-10-01\"") && text.contains("\"version\": 1"), "{text}");

        // A damaged file: start empty, say why, and replace it on the next save.
        std::fs::write(&path, "{not json").unwrap();
        let (damaged, error) = Sky::open(SkyConfig::default(), path.clone());
        assert!(error.unwrap().contains("is not valid"));
        assert_eq!(damaged.usage(), UsageFile::default());
        damaged.allow_press(3, now, &zone).unwrap();
        damaged.save().unwrap();
        assert_eq!(Sky::open(SkyConfig::default(), path).0.today(3, now, &zone).presses, 1);
    }
}
