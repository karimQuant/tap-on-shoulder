//! Idle ledger: `${CLAUDE_PLUGIN_DATA}/stats/waits.jsonl`, one line per
//! finished wait, and `tap stats`.
//!
//! - Only `needs_decision`, `needs_permission`, and `blocked` episodes are
//!   recorded. Lines carry timestamps, the category, a fixed-vocabulary close
//!   reason, and (unless `privacy = "category-only"`) the project folder name.
//!   Never message text, summaries, or tool names.
//! - [`record`] is the single funnel. It never fails: every error is logged
//!   as `kind=stats_write_failed` and swallowed, so a stats problem never
//!   delays or blocks a tap.
//! - Appends hold `stats/waits.lock` (bounded non-blocking wait, ≤ 50 ms), no
//!   fsync. Past 1 MiB the file is rewritten under the same lock keeping the
//!   last 90 days. If that rewrite fails, the next append retries it first
//!   and appends nothing while it keeps failing; past 4 MiB (grown by
//!   something else) appends stop. Either way `kind=stats_write_failed` is
//!   logged and the file never grows without bound; `tap stats` still reads
//!   it.

use std::collections::HashMap;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::{Config, Privacy};
use crate::contract::{Category, Episode, EpisodeStatus};
use crate::log::Entry;
use crate::state::{self, FileLock};

pub const LEDGER_VERSION: u32 = 1;
/// Rotate past this size.
pub const ROTATE_BYTES: u64 = 1 << 20;
/// Rotation keeps lines closed within this many seconds.
pub const KEEP_S: i64 = 90 * 86_400;
/// If 90 days still exceed [`ROTATE_BYTES`], keep only the newest lines up to
/// this size, so rotation does not run on every append.
const ROTATE_TARGET_BYTES: usize = 768 << 10;
/// Each wait counts at most this long toward blocked-on-you time.
pub const CAP_S: i64 = 2 * 3600;
/// Never append to a ledger larger than this (4 × [`ROTATE_BYTES`]).
pub const HARD_CAP_BYTES: u64 = 4 * ROTATE_BYTES;
const LOCK_BUDGET: Duration = Duration::from_millis(50);
const LOCK_STEP: Duration = Duration::from_millis(5);
/// Reading for rotation or `tap stats`; anything bigger is refused.
const READ_MAX_BYTES: u64 = 64 << 20;

/// One ledger line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub v: u32,
    pub opened: i64,
    pub closed: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified: Option<i64>,
    pub category: Category,
    pub reason: String,
    pub user: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The tap was sent early because the user was away (set by presence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub early: Option<bool>,
}

/// Close reasons and whether each is a user action (counts toward
/// blocked-on-you time). Anything else is never written.
const REASONS: &[(&str, bool)] = &[
    ("user_prompt", true),
    ("tool_done", true),
    ("permission_denied", true),
    ("elicitation_result", true),
    ("rebind", true),
    ("session_end", false),
    ("session_gone", false),
    ("session_start", false),
    ("stop", false),
    ("stop_failure", false),
    ("background_task", false),
    ("activity", false),
];

fn reason_vocab(reason: &str) -> Option<(&'static str, bool)> {
    REASONS.iter().find(|(r, _)| *r == reason).copied()
}

/// Whether `reason` is a user action that finishes a tapped episode.
pub fn is_user_action(reason: &str) -> bool {
    reason_vocab(reason).is_some_and(|(_, u)| u)
}

fn recorded_category(c: Category) -> bool {
    matches!(
        c,
        Category::NeedsDecision | Category::NeedsPermission | Category::Blocked
    )
}

/// Not yet recorded: still live, or tapped but not finished.
pub fn unrecorded(ep: &Episode) -> bool {
    match ep.status {
        EpisodeStatus::Open | EpisodeStatus::Sending => true,
        EpisodeStatus::Notified | EpisodeStatus::SentUncertain | EpisodeStatus::Failed => {
            ep.finished_reason.is_none()
        }
        EpisodeStatus::Closed => false,
    }
}

/// The ledger reason if one hook turned `before` into `after` by closing a
/// live episode or finishing a tapped one.
pub fn ended_by(before: Option<&Episode>, after: Option<&Episode>) -> Option<String> {
    let (b, a) = (before?, after?);
    if a.id != b.id {
        return None;
    }
    let live = matches!(b.status, EpisodeStatus::Open | EpisodeStatus::Sending);
    if live && a.status == EpisodeStatus::Closed {
        return a.closed_reason.clone();
    }
    if b.finished_reason.is_none() {
        return a.finished_reason.clone();
    }
    None
}

/// `${CLAUDE_PLUGIN_DATA}/stats/waits.jsonl` (the path only; nothing created).
pub fn ledger_path() -> Option<PathBuf> {
    state::data_dir().map(|d| d.join("stats").join("waits.jsonl"))
}

/// Record one finished wait. Never fails; never blocks longer than the lock
/// budget.
pub fn record(ep: &Episode, reason: &str, closed: i64, project: &str, cfg: &Config) {
    if !cfg.stats_enabled || !recorded_category(ep.category) {
        return;
    }
    let Some((reason, user)) = reason_vocab(reason) else {
        return;
    };
    let project = (cfg.ntfy.privacy != Privacy::CategoryOnly && !project.is_empty())
        .then(|| project.to_string());
    let line = Line {
        v: LEDGER_VERSION,
        opened: ep.opened_at,
        closed,
        notified: ep.notified_at,
        category: ep.category,
        reason: reason.to_string(),
        user,
        project,
        early: ep.sent_early.then_some(true),
    };
    if append(&line).is_err() {
        Entry::new("stats").kind("stats_write_failed").write();
    }
}

fn lock(path: &std::path::Path) -> io::Result<FileLock> {
    let start = Instant::now();
    loop {
        if let Some(l) = FileLock::try_acquire(path)? {
            return Ok(l);
        }
        if start.elapsed() + LOCK_STEP > LOCK_BUDGET {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "lock_timeout"));
        }
        std::thread::sleep(LOCK_STEP);
    }
}

fn open_append(path: &std::path::Path) -> io::Result<(std::fs::File, u64)> {
    let f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let md = f.metadata()?;
    if !md.file_type().is_file() {
        return Err(io::Error::other("not_a_file"));
    }
    Ok((f, md.len()))
}

fn append(line: &Line) -> io::Result<()> {
    let data = state::data_dir().ok_or_else(|| io::Error::other("no_data_dir"))?;
    if !data.is_dir() {
        return Err(io::Error::other("no_data_dir"));
    }
    let dir = state::private_subdir(&data, "stats")?;
    let _lock = lock(&dir.join("waits.lock"))?;
    let path = dir.join("waits.jsonl");
    let (mut f, mut len) = open_append(&path)?;
    if len > HARD_CAP_BYTES {
        return Err(io::Error::other("ledger_over_cap"));
    }
    if len > ROTATE_BYTES {
        // An earlier rotation failed: retry it, and append nothing until it
        // works, so the file cannot grow without bound.
        drop(f);
        rotate(&path, line.closed)?;
        (f, len) = open_append(&path)?;
        if len > ROTATE_BYTES {
            return Err(io::Error::other("ledger_over_cap"));
        }
    }
    let mut bytes = serde_json::to_vec(line).map_err(io::Error::other)?;
    bytes.push(b'\n');
    f.write_all(&bytes)?;
    if len + bytes.len() as u64 > ROTATE_BYTES {
        drop(f);
        rotate(&path, line.closed)?;
    }
    Ok(())
}

/// Rewrite keeping lines closed in the last 90 days (caller holds the lock).
fn rotate(path: &std::path::Path, now: i64) -> io::Result<()> {
    let bytes = state::read_nofollow(path, READ_MAX_BYTES)?;
    let text = String::from_utf8_lossy(&bytes);
    let cutoff = now - KEEP_S;
    let mut keep: Vec<&str> = text
        .lines()
        .filter(|l| {
            serde_json::from_str::<Line>(l)
                .is_ok_and(|x| x.v == LEDGER_VERSION && x.closed >= cutoff)
        })
        .collect();
    let mut size: usize = keep.iter().map(|l| l.len() + 1).sum();
    if size as u64 > ROTATE_BYTES {
        let mut drop_n = 0;
        while size > ROTATE_TARGET_BYTES && drop_n < keep.len() {
            size -= keep[drop_n].len() + 1;
            drop_n += 1;
        }
        keep.drain(..drop_n);
    }
    let mut out = String::with_capacity(size);
    for l in keep {
        out.push_str(l);
        out.push('\n');
    }
    state::write_atomic(path, out.as_bytes())
}

// ---------------------------------------------------------------- report

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectTotal {
    pub project: String,
    pub blocked_s: i64,
    pub waits: u64,
}

/// `tap stats --json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub days: u32,
    /// Σ min(max(closed − opened, 0), 2 h) over user-closed waits.
    pub blocked_s: i64,
    /// User-closed waits in the window.
    pub waits: u64,
    /// Of those, how many were tapped.
    pub tapped: u64,
    /// Median minutes from tap to reply, over tapped waits.
    pub median_reply_min: Option<f64>,
    pub top_projects: Vec<ProjectTotal>,
    /// Waits ended without a user action (session end, crash, new turn).
    pub not_user: u64,
    /// Corrupt or unknown lines.
    pub skipped: u64,
}

/// Aggregate ledger `text` for the `days` before `now`, by `closed`.
pub fn report(text: &str, now: i64, days: u32) -> Report {
    let since = now - i64::from(days) * 86_400;
    let mut r = Report {
        days,
        blocked_s: 0,
        waits: 0,
        tapped: 0,
        median_reply_min: None,
        top_projects: Vec::new(),
        not_user: 0,
        skipped: 0,
    };
    let mut replies: Vec<i64> = Vec::new();
    let mut projects: HashMap<String, (i64, u64)> = HashMap::new();
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let line = match serde_json::from_str::<Line>(raw) {
            Ok(l)
                if l.v == LEDGER_VERSION
                    && reason_vocab(&l.reason).is_some_and(|(_, u)| u == l.user) =>
            {
                l
            }
            _ => {
                r.skipped += 1;
                continue;
            }
        };
        if !recorded_category(line.category) {
            r.skipped += 1;
            continue;
        }
        if line.closed < since || line.closed > now {
            continue;
        }
        if !line.user {
            r.not_user += 1;
            continue;
        }
        let wait = (line.closed - line.opened).clamp(0, CAP_S);
        r.blocked_s += wait;
        r.waits += 1;
        if let Some(n) = line.notified {
            r.tapped += 1;
            replies.push((line.closed - n).max(0));
        }
        if let Some(p) = line.project {
            let e = projects.entry(p).or_default();
            e.0 += wait;
            e.1 += 1;
        }
    }
    if !replies.is_empty() {
        replies.sort_unstable();
        let n = replies.len();
        let med = if n % 2 == 1 {
            replies[n / 2] as f64
        } else {
            (replies[n / 2 - 1] + replies[n / 2]) as f64 / 2.0
        };
        r.median_reply_min = Some((med / 60.0 * 10.0).round() / 10.0);
    }
    let mut top: Vec<ProjectTotal> = projects
        .into_iter()
        .map(|(project, (blocked_s, waits))| ProjectTotal {
            project,
            blocked_s,
            waits,
        })
        .collect();
    top.sort_by(|a, b| {
        b.blocked_s
            .cmp(&a.blocked_s)
            .then(a.project.cmp(&b.project))
    });
    top.truncate(3);
    r.top_projects = top;
    r
}

/// `4 h 10 m`, `12 m`, `0 m`.
pub fn human_duration(s: i64) -> String {
    let m = s.max(0) / 60;
    if m >= 60 {
        format!("{} h {} m", m / 60, m % 60)
    } else {
        format!("{m} m")
    }
}

/// Number of lines in the ledger, if it exists and is readable.
pub fn line_count() -> Option<usize> {
    let bytes = state::read_nofollow(&ledger_path()?, READ_MAX_BYTES).ok()?;
    Some(bytes.iter().filter(|b| **b == b'\n').count())
}

/// How to turn the ledger off, for setup and doctor.
pub const OFF_HINT: &str = "turn off with `[stats] enabled = false` in the user config";

/// How to turn the ledger on (it is off unless the user config says
/// `[stats] enabled = true`), for setup, doctor, and `tap stats`.
pub const ON_HINT: &str =
    "turn on with `[stats] enabled = true` in the user config (a missing key means off)";

/// `tap stats [--days N] [--json]`. Returns the exit code.
pub fn run_cli(args: &[String]) -> i32 {
    const USAGE: &str = "usage: tap stats [--days N] [--json]";
    let mut days: u32 = 7;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--days" => match it.next().and_then(|d| d.parse::<u32>().ok()) {
                Some(d) if (1..=3650).contains(&d) => days = d,
                _ => {
                    eprintln!("--days needs a number of days between 1 and 3650");
                    return 2;
                }
            },
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let cfg = crate::config::load(None).config;
    let text = match ledger_path() {
        None => String::new(),
        Some(p) => match state::read_nofollow(&p, READ_MAX_BYTES) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(_) => {
                eprintln!("cannot read the idle ledger at {}", p.display());
                return 1;
            }
        },
    };
    let r = report(&text, state::now(), days);
    if json {
        println!(
            "{}",
            serde_json::to_string(&r).unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }
    let period = if days == 1 {
        "day".to_string()
    } else {
        format!("{days} days")
    };
    println!(
        "Last {period}: your agents sat blocked on you for {} across {} wait{}.",
        human_duration(r.blocked_s),
        r.waits,
        if r.waits == 1 { "" } else { "s" }
    );
    println!("tapped: {} of {}", r.tapped, r.waits);
    match r.median_reply_min {
        Some(m) => println!("median from tap to reply: {m} min"),
        None => println!("median from tap to reply: -"),
    }
    for p in &r.top_projects {
        println!(
            "project: {} {} ({} wait{})",
            p.project,
            human_duration(p.blocked_s),
            p.waits,
            if p.waits == 1 { "" } else { "s" }
        );
    }
    if r.not_user > 0 {
        println!(
            "not counted: {} wait(s) ended without a reply (session ended, new turn)",
            r.not_user
        );
    }
    if r.skipped > 0 {
        println!("skipped: {} unreadable line(s)", r.skipped);
    }
    println!("Each wait counts at most 2 h. Local only, no message text.");
    if !cfg.stats_enabled {
        println!("ledger: off; nothing new is recorded; {ON_HINT}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_maps_user_actions() {
        assert!(is_user_action("user_prompt"));
        assert!(is_user_action("rebind"));
        assert!(!is_user_action("session_end"));
        assert!(!is_user_action("activity"));
        assert!(!is_user_action("nonsense"));
    }

    #[test]
    fn human_durations() {
        assert_eq!(human_duration(15_000), "4 h 10 m");
        assert_eq!(human_duration(59), "0 m");
        assert_eq!(human_duration(-5), "0 m");
        assert_eq!(human_duration(720), "12 m");
    }

    #[test]
    fn report_rejects_mismatched_user_flag() {
        let now = 1_000_000;
        let t = format!(
            "{{\"v\":1,\"opened\":{},\"closed\":{now},\"category\":\"blocked\",\"reason\":\"session_end\",\"user\":true}}\n",
            now - 60
        );
        let r = report(&t, now, 7);
        assert_eq!(r.skipped, 1);
        assert_eq!(r.waits, 0);
    }
}
