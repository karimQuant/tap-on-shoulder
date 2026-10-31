//! Configuration (plan D8, `docs/configuration.md`).
//!
//! 1. User config: `${XDG_CONFIG_HOME:-~/.config}/tap-on-shoulder/config.toml`.
//! 2. Project config: `<cwd>/.claude/tap-on-shoulder.toml` (≤ 64 KiB). Only
//!    `grace.*` (clamped to the user's value or lower) and `alerts.completion`
//!    are honoured; every other key (including `stats.*` and `presence.*`)
//!    is ignored with a warning. `grace.away` is also allowed there,
//!    shorten-only.
//! 3. `TAP_GRACE_OVERRIDE` replaces every category grace period, only with
//!    `TAP_DEV=1`. It never raises `grace.away`; it can only clamp it down.
//!
//! `grace.away` never exceeds any category grace: a user value above one is
//! a config error, and every layer's result is clamped to the minimum.

use std::path::{Path, PathBuf};

use crate::contract::Category;

pub const GRACE_MIN_S: u64 = 5;
pub const GRACE_MAX_S: u64 = 24 * 3600;
pub const DEFAULT_GRACE_S: u64 = 300;
/// `grace.away`: grace when the user is away (screen locked or idle).
pub const DEFAULT_AWAY_S: u64 = 30;
pub const AWAY_MIN_S: u64 = 1;
/// `presence.idle_away`: input idle this long counts as away.
pub const DEFAULT_IDLE_AWAY_S: u64 = 300;
pub const IDLE_AWAY_MIN_S: u64 = 10;
pub const DEFAULT_SERVER: &str = "https://ntfy.sh";
pub const PROJECT_MAX_BYTES: u64 = 64 << 10;
const USER_MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    Summary,
    CategoryOnly,
}

/// `classifier.mode`: `model` uses the embedded head when the build has one
/// (else the rules); `rules` always uses the rules classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifierMode {
    Model,
    Rules,
}

/// `classifier.lean`: `alert` applies head.json's `lean_threshold`;
/// `balanced` is plain argmax over the head's probabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lean {
    Alert,
    Balanced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ntfy {
    pub server: String,
    pub topic: Option<String>,
    pub token_env: Option<String>,
    pub privacy: Privacy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grace {
    pub question: u64,
    pub permission: u64,
    pub blocked: u64,
    /// Grace while the user is away; ≤ every category grace.
    pub away: u64,
}

impl Grace {
    fn min_category(&self) -> u64 {
        self.question.min(self.permission).min(self.blocked)
    }
}

/// `presence.mode`: `auto` probes presence; `off` is v0.1 behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceMode {
    Auto,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    pub mode: PresenceMode,
    pub idle_away: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub grace: Grace,
    pub alerts_completion: bool,
    pub alerts_info: bool,
    pub classifier_mode: ClassifierMode,
    pub classifier_lean: Lean,
    pub ntfy: Ntfy,
    pub training_collect: bool,
    /// `[stats] enabled`: the local idle ledger (user config only).
    pub stats_enabled: bool,
    pub presence: Presence,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            grace: Grace {
                question: DEFAULT_GRACE_S,
                permission: DEFAULT_GRACE_S,
                blocked: DEFAULT_GRACE_S,
                away: DEFAULT_AWAY_S,
            },
            alerts_completion: false,
            alerts_info: false,
            classifier_mode: ClassifierMode::Model,
            classifier_lean: Lean::Alert,
            ntfy: Ntfy {
                server: DEFAULT_SERVER.into(),
                topic: None,
                token_env: None,
                privacy: Privacy::Summary,
            },
            training_collect: false,
            stats_enabled: true,
            presence: Presence {
                mode: PresenceMode::Auto,
                idle_away: DEFAULT_IDLE_AWAY_S,
            },
        }
    }
}

impl Config {
    /// Grace in seconds for a category. `done` and `info` use `grace.question`.
    pub fn grace_for(&self, c: Category) -> u64 {
        match c {
            Category::NeedsPermission => self.grace.permission,
            Category::Blocked => self.grace.blocked,
            Category::NeedsDecision | Category::Done | Category::Info => self.grace.question,
        }
    }

    /// Whether an episode of this category may tap at all.
    pub fn taps(&self, c: Category) -> bool {
        match c {
            Category::Done => self.alerts_completion,
            Category::Info => self.alerts_info,
            _ => true,
        }
    }
}

/// A configuration problem. Messages name keys, never values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub source: &'static str,
    pub key: String,
    pub problem: &'static str,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} config: {}: {}", self.source, self.key, self.problem)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub config: Config,
    /// Keys that were ignored (unknown, or not allowed in a project file).
    pub warnings: Vec<Issue>,
    /// Invalid values or unparseable files. Defaults were used instead.
    pub errors: Vec<Issue>,
    /// Whether a user config file exists.
    pub user_file: bool,
}

impl Loaded {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// `TAP_DEV=1`.
pub fn dev() -> bool {
    std::env::var("TAP_DEV").as_deref() == Ok("1")
}

/// Test-only knobs (`TAP_DEV_FAST`, `TAP_TEST_*`): debug builds with `TAP_DEV=1`.
/// Release binaries ignore them, so the environment cannot steer delivery.
pub fn test_knobs() -> bool {
    cfg!(debug_assertions) && dev()
}

/// `TAP_DEV_FAST=1`: shortens retry backoff and permission postponement so
/// tests run in seconds.
pub fn dev_fast() -> bool {
    test_knobs() && std::env::var("TAP_DEV_FAST").as_deref() == Ok("1")
}

pub fn user_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("tap-on-shoulder").join("config.toml"))
}

pub fn project_config_path(cwd: &Path) -> PathBuf {
    cwd.join(".claude").join("tap-on-shoulder.toml")
}

/// Parse `90s`, `5m`, `1h` (a bare number means seconds) within `[min, max]`.
pub fn parse_duration(s: &str, min: u64, max: u64) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last()? {
        (i, 's') => (&s[..i], 1),
        (i, 'm') => (&s[..i], 60),
        (i, 'h') => (&s[..i], 3600),
        _ => (s, 1),
    };
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = num.parse::<u64>().ok()?.checked_mul(mult)?;
    (min..=max).contains(&v).then_some(v)
}

/// Load the effective config. `cwd` enables the project file.
pub fn load(cwd: Option<&Path>) -> Loaded {
    let mut out = Loaded::default();
    if let Some(path) = user_config_path() {
        match crate::state::read_nofollow(&path, USER_MAX_BYTES) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => out.errors.push(issue("user", "<file>", "unreadable")),
            Ok(bytes) => {
                out.user_file = true;
                match parse_table(&bytes) {
                    Some(t) => apply_user(&mut out, &t),
                    None => out.errors.push(issue("user", "<file>", "not valid TOML")),
                }
            }
        }
    }
    if let Some(cwd) = cwd.filter(|c| c.is_absolute()) {
        apply_project_file(&mut out, &project_config_path(cwd));
    }
    if let Ok(v) = std::env::var("TAP_GRACE_OVERRIDE") {
        if dev() {
            match parse_duration(&v, 1, GRACE_MAX_S) {
                Some(g) => {
                    out.config.grace = Grace {
                        question: g,
                        permission: g,
                        blocked: g,
                        away: out.config.grace.away.min(g),
                    }
                }
                None => out
                    .warnings
                    .push(issue("env", "TAP_GRACE_OVERRIDE", "invalid, ignored")),
            }
        } else {
            out.warnings.push(issue(
                "env",
                "TAP_GRACE_OVERRIDE",
                "ignored without TAP_DEV=1",
            ));
        }
    }
    let g = &mut out.config.grace;
    g.away = g.away.min(g.min_category());
    out
}

fn issue(source: &'static str, key: &str, problem: &'static str) -> Issue {
    Issue {
        source,
        key: key.to_string(),
        problem,
    }
}

fn parse_table(bytes: &[u8]) -> Option<toml::Table> {
    let text = std::str::from_utf8(bytes).ok()?;
    toml::from_str::<toml::Table>(text).ok()
}

/// Flatten a table into `(dotted.key, value)` leaves.
fn leaves(t: &toml::Table) -> Vec<(String, toml::Value)> {
    fn walk(prefix: &str, t: &toml::Table, out: &mut Vec<(String, toml::Value)>) {
        for (k, v) in t {
            let key = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                toml::Value::Table(sub) => walk(&key, sub, out),
                other => out.push((key, other.clone())),
            }
        }
    }
    let mut out = Vec::new();
    walk("", t, &mut out);
    out
}

/// Keys are printed in warnings; keep them short and printable.
fn display_key(k: &str) -> String {
    let s: String = k
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(64)
        .collect();
    if s.is_empty() {
        "<key>".into()
    } else {
        s
    }
}

fn apply_user(out: &mut Loaded, t: &toml::Table) {
    let mut away_set = false;
    for (key, v) in leaves(t) {
        let c = &mut out.config;
        let bad = |out: &mut Vec<Issue>, p| out.push(issue("user", &key, p));
        match key.as_str() {
            "grace.question" | "grace.permission" | "grace.blocked" => {
                match v
                    .as_str()
                    .and_then(|s| parse_duration(s, GRACE_MIN_S, GRACE_MAX_S))
                {
                    Some(g) => match key.as_str() {
                        "grace.question" => c.grace.question = g,
                        "grace.permission" => c.grace.permission = g,
                        _ => c.grace.blocked = g,
                    },
                    None => bad(&mut out.errors, "expected a duration between 5s and 24h"),
                }
            }
            "grace.away" => match v
                .as_str()
                .and_then(|s| parse_duration(s, AWAY_MIN_S, GRACE_MAX_S))
            {
                Some(g) => {
                    c.grace.away = g;
                    away_set = true;
                }
                None => bad(&mut out.errors, "expected a duration between 1s and 24h"),
            },
            "presence.mode" => match v.as_str() {
                Some("auto") => c.presence.mode = PresenceMode::Auto,
                Some("off") => c.presence.mode = PresenceMode::Off,
                _ => bad(&mut out.errors, "expected \"auto\" or \"off\""),
            },
            "presence.idle_away" => match v
                .as_str()
                .and_then(|s| parse_duration(s, IDLE_AWAY_MIN_S, GRACE_MAX_S))
            {
                Some(d) => c.presence.idle_away = d,
                None => bad(&mut out.errors, "expected a duration between 10s and 24h"),
            },
            "alerts.completion" | "alerts.info" => match v.as_bool() {
                Some(b) if key == "alerts.completion" => c.alerts_completion = b,
                Some(b) => c.alerts_info = b,
                None => bad(&mut out.errors, "expected true or false"),
            },
            "alerts.repeat" => {
                if v.as_str() != Some("off") {
                    bad(&mut out.warnings, "only \"off\" is supported in v0.1");
                }
            }
            "classifier.mode" => match v.as_str() {
                Some("model") => c.classifier_mode = ClassifierMode::Model,
                Some("rules") => c.classifier_mode = ClassifierMode::Rules,
                _ => bad(&mut out.errors, "expected \"model\" or \"rules\""),
            },
            "classifier.lean" => match v.as_str() {
                Some("alert") => c.classifier_lean = Lean::Alert,
                Some("balanced") => c.classifier_lean = Lean::Balanced,
                _ => bad(&mut out.errors, "expected \"alert\" or \"balanced\""),
            },
            "notify.ntfy.server" => match v.as_str() {
                Some(s) if s.starts_with("https://") || s.starts_with("http://") => {
                    c.ntfy.server = s.trim_end_matches('/').to_string()
                }
                _ => bad(&mut out.errors, "expected an http(s) URL"),
            },
            "notify.ntfy.topic" => match v.as_str() {
                Some(s) if valid_topic(s) => c.ntfy.topic = Some(s.to_string()),
                _ => bad(&mut out.errors, "expected 1-64 chars of [A-Za-z0-9_-]"),
            },
            "notify.ntfy.token_env" => match v.as_str() {
                Some(s) if valid_env_name(s) => c.ntfy.token_env = Some(s.to_string()),
                _ => bad(&mut out.errors, "expected an environment variable name"),
            },
            "notify.ntfy.privacy" => match v.as_str() {
                Some("summary") => c.ntfy.privacy = Privacy::Summary,
                Some("category-only") => c.ntfy.privacy = Privacy::CategoryOnly,
                _ => bad(&mut out.errors, "expected \"summary\" or \"category-only\""),
            },
            "training.collect" => match v.as_bool() {
                Some(b) => c.training_collect = b,
                None => bad(&mut out.errors, "expected true or false"),
            },
            "stats.enabled" => match v.as_bool() {
                Some(b) => c.stats_enabled = b,
                None => bad(&mut out.errors, "expected true or false"),
            },
            _ => out
                .warnings
                .push(issue("user", &display_key(&key), "unknown key, ignored")),
        }
    }
    let g = &mut out.config.grace;
    if away_set && g.away > g.min_category() {
        out.errors.push(issue(
            "user",
            "grace.away",
            "must not exceed any category grace",
        ));
        g.away = DEFAULT_AWAY_S;
    }
}

fn apply_project_file(out: &mut Loaded, path: &Path) {
    let bytes = match crate::state::read_nofollow(path, PROJECT_MAX_BYTES) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            out.warnings.push(issue(
                "project",
                "<file>",
                "unreadable or over 64 KiB, ignored",
            ));
            return;
        }
        Ok(b) => b,
    };
    let Some(t) = parse_table(&bytes) else {
        out.warnings
            .push(issue("project", "<file>", "not valid TOML, ignored"));
        return;
    };
    for (key, v) in leaves(&t) {
        let c = &mut out.config;
        match key.as_str() {
            "grace.question" | "grace.permission" | "grace.blocked" => {
                let Some(g) = v
                    .as_str()
                    .and_then(|s| parse_duration(s, GRACE_MIN_S, GRACE_MAX_S))
                else {
                    out.warnings
                        .push(issue("project", &key, "invalid duration, ignored"));
                    continue;
                };
                let slot = match key.as_str() {
                    "grace.question" => &mut c.grace.question,
                    "grace.permission" => &mut c.grace.permission,
                    _ => &mut c.grace.blocked,
                };
                *slot = (*slot).min(g);
            }
            "grace.away" => match v
                .as_str()
                .and_then(|s| parse_duration(s, AWAY_MIN_S, GRACE_MAX_S))
            {
                Some(g) => c.grace.away = c.grace.away.min(g),
                None => out
                    .warnings
                    .push(issue("project", &key, "invalid duration, ignored")),
            },
            "alerts.completion" => match v.as_bool() {
                Some(b) => c.alerts_completion = b,
                None => {
                    out.warnings
                        .push(issue("project", &key, "expected true or false, ignored"))
                }
            },
            _ => out.warnings.push(issue(
                "project",
                &display_key(&key),
                "not allowed in a project file, ignored",
            )),
        }
    }
}

pub fn valid_topic(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_env_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s", 5, GRACE_MAX_S), Some(90));
        assert_eq!(parse_duration("5m", 5, GRACE_MAX_S), Some(300));
        assert_eq!(parse_duration("1h", 5, GRACE_MAX_S), Some(3600));
        assert_eq!(parse_duration("24h", 5, GRACE_MAX_S), Some(86400));
        assert_eq!(parse_duration("25h", 5, GRACE_MAX_S), None);
        assert_eq!(parse_duration("4s", 5, GRACE_MAX_S), None);
        assert_eq!(parse_duration("1s", 1, GRACE_MAX_S), Some(1));
        assert_eq!(parse_duration("-5m", 5, GRACE_MAX_S), None);
        assert_eq!(parse_duration("m", 5, GRACE_MAX_S), None);
        assert_eq!(parse_duration("5x", 5, GRACE_MAX_S), None);
    }

    #[test]
    fn project_only_lowers_grace_and_sets_completion() {
        let mut l = Loaded::default();
        l.config.grace.question = 600;
        let text = "[grace]\nquestion = \"1h\"\npermission = \"10s\"\n[alerts]\ncompletion = true\ninfo = true\n[notify.ntfy]\nserver = \"https://evil.example\"\ntopic = \"evil\"\n[training]\ncollect = true\n[stats]\nenabled = false\n";
        let dir =
            std::env::temp_dir().join(format!("tap-cfg-unit-{}", crate::state::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("p.toml");
        std::fs::write(&p, text).unwrap();
        apply_project_file(&mut l, &p);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(l.config.grace.question, 600, "clamped to user value");
        assert_eq!(l.config.grace.permission, 10);
        assert!(l.config.alerts_completion);
        assert!(!l.config.alerts_info);
        assert_eq!(l.config.ntfy, Config::default().ntfy);
        assert!(!l.config.training_collect);
        assert!(l.config.stats_enabled);
        let ignored: Vec<_> = l.warnings.iter().map(|w| w.key.as_str()).collect();
        assert!(ignored.contains(&"notify.ntfy.server"));
        assert!(ignored.contains(&"notify.ntfy.topic"));
        assert!(ignored.contains(&"training.collect"));
        assert!(ignored.contains(&"alerts.info"));
        assert!(ignored.contains(&"stats.enabled"));
    }

    #[test]
    fn user_config_values() {
        let mut l = Loaded::default();
        let t: toml::Table = toml::from_str(
            "[grace]\nquestion = \"2m\"\nblocked = \"bogus\"\n[notify.ntfy]\nserver = \"https://ntfy.example/\"\ntopic = \"tap-abc\"\nprivacy = \"category-only\"\n[weird]\nx = 1\n",
        )
        .unwrap();
        apply_user(&mut l, &t);
        assert_eq!(l.config.grace.question, 120);
        assert_eq!(l.config.grace.blocked, DEFAULT_GRACE_S);
        assert_eq!(l.config.ntfy.server, "https://ntfy.example");
        assert_eq!(l.config.ntfy.topic.as_deref(), Some("tap-abc"));
        assert_eq!(l.config.ntfy.privacy, Privacy::CategoryOnly);
        assert_eq!(l.errors.len(), 1);
        assert_eq!(l.warnings.len(), 1);
    }

    #[test]
    fn classifier_keys_user_only() {
        let mut l = Loaded::default();
        assert_eq!(l.config.classifier_mode, ClassifierMode::Model);
        assert_eq!(l.config.classifier_lean, Lean::Alert);
        let t: toml::Table =
            toml::from_str("[classifier]\nmode = \"rules\"\nlean = \"balanced\"\n").unwrap();
        apply_user(&mut l, &t);
        assert_eq!(l.config.classifier_mode, ClassifierMode::Rules);
        assert_eq!(l.config.classifier_lean, Lean::Balanced);
        assert!(l.errors.is_empty());

        let t: toml::Table = toml::from_str("[classifier]\nmode = \"x\"\n").unwrap();
        apply_user(&mut l, &t);
        assert_eq!(l.errors.len(), 1);

        let mut l = Loaded::default();
        let dir =
            std::env::temp_dir().join(format!("tap-cfg-unit-{}", crate::state::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("p.toml");
        std::fs::write(&p, "[classifier]\nmode = \"rules\"\nlean = \"balanced\"\n").unwrap();
        apply_project_file(&mut l, &p);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(l.config.classifier_mode, ClassifierMode::Model);
        assert_eq!(l.config.classifier_lean, Lean::Alert);
        assert_eq!(l.warnings.len(), 2);
    }

    #[test]
    fn grace_away_and_presence() {
        let mut l = Loaded::default();
        assert_eq!(l.config.grace.away, DEFAULT_AWAY_S);
        assert_eq!(l.config.presence.mode, PresenceMode::Auto);
        let t: toml::Table = toml::from_str(
            "[grace]\naway = \"10s\"\n[presence]\nmode = \"off\"\nidle_away = \"2m\"\n",
        )
        .unwrap();
        apply_user(&mut l, &t);
        assert!(l.errors.is_empty(), "{:?}", l.errors);
        assert_eq!(l.config.grace.away, 10);
        assert_eq!(l.config.presence.mode, PresenceMode::Off);
        assert_eq!(l.config.presence.idle_away, 120);

        // Above a category grace: error, default used.
        let mut l = Loaded::default();
        let t: toml::Table = toml::from_str("[grace]\nquestion = \"1m\"\naway = \"2m\"\n").unwrap();
        apply_user(&mut l, &t);
        assert_eq!(l.errors.len(), 1);
        assert_eq!(l.errors[0].key, "grace.away");
        assert_eq!(l.config.grace.away, DEFAULT_AWAY_S);

        let mut l = Loaded::default();
        let t: toml::Table =
            toml::from_str("[grace]\naway = \"0s\"\n[presence]\nmode = \"on\"\n").unwrap();
        apply_user(&mut l, &t);
        assert_eq!(l.errors.len(), 2);
    }

    #[test]
    fn project_may_only_shorten_grace_away() {
        let dir =
            std::env::temp_dir().join(format!("tap-cfg-unit-{}", crate::state::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("p.toml");
        std::fs::write(
            &p,
            "[grace]\naway = \"5s\"\n[presence]\nmode = \"off\"\nidle_away = \"10s\"\n",
        )
        .unwrap();
        let mut l = Loaded::default();
        apply_project_file(&mut l, &p);
        assert_eq!(l.config.grace.away, 5);
        assert_eq!(l.config.presence, Config::default().presence);
        let ignored: Vec<_> = l.warnings.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(ignored, ["presence.idle_away", "presence.mode"]);

        std::fs::write(&p, "[grace]\naway = \"2m\"\n").unwrap();
        let mut l = Loaded::default();
        apply_project_file(&mut l, &p);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(l.config.grace.away, DEFAULT_AWAY_S, "cannot lengthen");
    }
}
