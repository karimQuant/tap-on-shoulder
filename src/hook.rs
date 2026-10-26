//! `tap hook <Event>`: event mapping (plan D2 + R3).
//!
//! Always exits 0. Stdout stays empty except for the `SessionStart` warning,
//! printed as `{"systemMessage":"..."}`. The `UserPromptSubmit` prompt text is
//! never read (it is not even a field of `HookInput`).

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::config::{self, Config};
use crate::contract::{
    Category, Classification, Episode, EpisodeStatus, HookInput, PendingTool, SessionState,
    MAX_PENDING_TOOLS, STDIN_MAX_BYTES, TEXT_MAX_CHARS,
};
use crate::log::Entry;
use crate::state::{self, Loaded, Store};

/// Prune state not touched for this long on `SessionStart`.
const PRUNE_AFTER: Duration = Duration::from_secs(24 * 3600);

pub const NO_TOPIC_WARNING: &str =
    "tap-on-shoulder: no ntfy topic configured, so no taps will be sent. Run `tap setup`.";

/// Events we understand, as static strings for the log.
fn event_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "SessionStart" => "SessionStart",
        "SessionEnd" => "SessionEnd",
        "UserPromptSubmit" => "UserPromptSubmit",
        "PreToolUse" => "PreToolUse",
        "PostToolUse" => "PostToolUse",
        "PostToolUseFailure" => "PostToolUseFailure",
        "PermissionRequest" => "PermissionRequest",
        "PermissionDenied" => "PermissionDenied",
        "Notification" => "Notification",
        "ElicitationResult" => "ElicitationResult",
        "Stop" => "Stop",
        "StopFailure" => "StopFailure",
        "SubagentStart" => "SubagentStart",
        "SubagentStop" => "SubagentStop",
        "PreCompact" => "PreCompact",
        _ => return None,
    })
}

/// Entry point. Returns what to print on stdout (usually nothing).
pub fn run(arg_event: &str) -> Option<String> {
    let event = event_name(arg_event).unwrap_or("unknown");
    let Some(input) = read_input() else {
        Entry::new(event).kind("bad_input").write();
        return None;
    };
    let Some(event) = event_name(&input.hook_event_name) else {
        Entry::new("unknown").kind("unknown_event").write();
        return None;
    };
    if !state::valid_session_id(&input.session_id) {
        Entry::new(event).kind("bad_session_id").write();
        return None;
    }
    handle(event, &input)
}

fn read_input() -> Option<HookInput> {
    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .take(STDIN_MAX_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > STDIN_MAX_BYTES {
        return None;
    }
    serde_json::from_slice(&buf).ok()
}

/// Canonical JSON: object keys sorted recursively, compact separators.
pub fn canonical_json(v: &serde_json::Value) -> String {
    fn write(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::Value::String((*k).clone()).to_string());
                    out.push(':');
                    write(&m[*k], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut s = String::new();
    write(v, &mut s);
    s
}

/// First 16 hex digits of sha256(canonical `tool_input`).
pub fn input_hash(v: Option<&serde_json::Value>) -> String {
    let canon = canonical_json(v.unwrap_or(&serde_json::Value::Null));
    let digest = Sha256::digest(canon.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn project_name(cwd: &str) -> String {
    let base = Path::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean = crate::sanitize::clean(&base);
    if clean.trim().is_empty() {
        "session".into()
    } else {
        clean
    }
}

fn is_decision_tool(name: &str) -> bool {
    matches!(name, "AskUserQuestion" | "ExitPlanMode")
}

/// Summary for a decision tool, from its input.
fn decision_summary(tool: &str, input: Option<&serde_json::Value>) -> String {
    let text = match tool {
        "AskUserQuestion" => input
            .and_then(|v| v.get("questions"))
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(|q| q.as_str())
            .unwrap_or("")
            .to_string(),
        _ => {
            let plan = input
                .and_then(|v| v.get("plan"))
                .and_then(|p| p.as_str())
                .unwrap_or("");
            plan.lines()
                .map(|l| l.trim().trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .map(|l| format!("plan ready: {l}"))
                .unwrap_or_else(|| "plan ready for review".into())
        }
    };
    crate::sanitize::clean(&text)
}

/// Kinds of open episode, for the close rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bound,
    Elicitation,
    Unbound,
}

const ELICITATION_TRIGGER: &str = "Notification:elicitation_dialog";

fn kind_of(ep: &Episode) -> Kind {
    if ep.trigger == ELICITATION_TRIGGER {
        Kind::Elicitation
    } else if ep.tool_use_id.is_some() {
        Kind::Bound
    } else {
        Kind::Unbound
    }
}

/// Mutable view of a session during one hook.
struct Ctx<'a> {
    event: &'static str,
    state: SessionState,
    cfg: &'a Config,
    now: i64,
    changed: bool,
    /// An arming event ran (opens, re-arms, or suppresses an episode).
    armed: bool,
}

impl Ctx<'_> {
    fn open_episode(&self) -> Option<&Episode> {
        self.state.episode.as_ref().filter(|e| e.status.is_open())
    }

    /// Close the episode if it is open or in flight and `pred` matches.
    /// A hook close always wins over an in-flight send (write guard).
    fn close_if(&mut self, reason: &'static str, pred: impl Fn(&Episode) -> bool) {
        let event = self.event;
        let sid = self.state.session_id.clone();
        if let Some(ep) = self.state.episode.as_mut() {
            let live = matches!(ep.status, EpisodeStatus::Open | EpisodeStatus::Sending);
            if live && pred(ep) {
                ep.status = EpisodeStatus::Closed;
                ep.closed_reason = Some(reason.into());
                self.changed = true;
                Entry::new(event)
                    .session(&sid)
                    .episode(&ep.id)
                    .transition("closed")
                    .category(ep.category)
                    .kind(reason)
                    .write();
            }
        }
    }

    fn clear_pending(&mut self) {
        if !self.state.pending_tools.is_empty() {
            self.state.pending_tools.clear();
            self.changed = true;
        }
    }

    /// Open or re-arm (plan R3).
    fn arm(&mut self, category: Category, summary: String, trigger: &str, tool: Option<String>) {
        let bound = tool.is_some() || trigger == ELICITATION_TRIGGER;
        let now = self.now;
        let sid = self.state.session_id.clone();
        let transition;
        if let Some(ep) = self.state.episode.as_mut().filter(|e| e.status.is_open()) {
            if bound || category.priority() > ep.category.priority() {
                ep.category = category;
                ep.summary = summary;
                ep.trigger = trigger.to_string();
                if bound {
                    ep.tool_use_id = tool;
                    ep.bound_at = Some(now);
                }
            }
            ep.due_at = ep.opened_at + self.cfg.grace_for(ep.category) as i64;
            transition = "rearmed";
        } else {
            let grace = self.cfg.grace_for(category) as i64;
            self.state.episode = Some(Episode {
                id: state::random_hex(8),
                status: EpisodeStatus::Open,
                category,
                summary,
                trigger: trigger.to_string(),
                bound_at: tool.as_ref().map(|_| now),
                tool_use_id: tool,
                opened_at: now,
                due_at: now + grace,
                confirmed_at: None,
                attempts: 0,
                retry_at: None,
                sending_since: None,
                postponed_s: 0,
                closed_reason: None,
            });
            self.state.claude = crate::proc::detect_claude();
            transition = "opened";
        }
        self.changed = true;
        self.armed = true;
        let ep = self.state.episode.as_mut().expect("armed episode");
        Entry::new(self.event)
            .session(&sid)
            .episode(&ep.id)
            .transition(transition)
            .category(ep.category)
            .write();
        if !self.cfg.taps(ep.category) {
            ep.status = EpisodeStatus::Closed;
            ep.closed_reason = Some("suppressed".into());
            Entry::new(self.event)
                .session(&sid)
                .episode(&ep.id)
                .transition("closed")
                .category(ep.category)
                .kind("suppressed")
                .write();
        }
    }
}

fn handle(event: &'static str, input: &HookInput) -> Option<String> {
    let sid = input.session_id.as_str();
    let store = match Store::open(true) {
        Ok(s) => s,
        Err(_) => {
            Entry::new(event).session(sid).kind("state_dir").write();
            return session_start_warning(event, input);
        }
    };
    let Ok(_lock) = store.lock(sid) else {
        Entry::new(event).session(sid).kind("lock").write();
        return None;
    };

    if event == "SessionEnd" {
        let _ = store.remove(sid);
        Entry::new(event)
            .session(sid)
            .transition("state_deleted")
            .write();
        return None;
    }

    let cwd = (!input.cwd.is_empty()).then(|| Path::new(input.cwd.as_str()));
    let loaded = config::load(cwd);
    for w in &loaded.warnings {
        let kind = match w.source {
            "project" => "project_key_ignored",
            "env" => "env_ignored",
            _ => "config_key_ignored",
        };
        Entry::new(event).session(sid).kind(kind).write();
    }
    let cfg = &loaded.config;
    let now = state::now();

    let (state, corrupt) = match store.load(sid) {
        Loaded::Ok(s) => (*s, false),
        Loaded::Missing => (state::new_state(sid, &project_name(&input.cwd)), false),
        Loaded::Corrupt => {
            Entry::new(event).session(sid).kind("corrupt_state").write();
            (state::new_state(sid, &project_name(&input.cwd)), true)
        }
    };
    let mut ctx = Ctx {
        event,
        state,
        cfg,
        now,
        changed: false,
        armed: false,
    };

    let mut out = None;
    let mut user_prompt = false;
    match event {
        "SessionStart" => {
            ctx.close_if("session_start", |_| true);
            ctx.clear_pending();
            store.prune(PRUNE_AFTER, sid);
            out = session_start_warning(event, input);
        }
        "UserPromptSubmit" => {
            ctx.close_if("user_prompt", |_| true);
            ctx.clear_pending();
            user_prompt = true;
        }
        "PreToolUse" => {
            let tool = input.tool_name.clone().unwrap_or_default();
            ctx.close_if("activity", |e| kind_of(e) == Kind::Unbound);
            if let Some(id) = input.tool_use_id.clone().filter(|i| !i.is_empty()) {
                let p = &mut ctx.state.pending_tools;
                p.retain(|t| t.tool_use_id != id);
                p.push(PendingTool {
                    tool_use_id: id.clone(),
                    tool_name: tool.clone(),
                    input_hash: input_hash(input.tool_input.as_ref()),
                });
                if p.len() > MAX_PENDING_TOOLS {
                    let drop = p.len() - MAX_PENDING_TOOLS;
                    p.drain(..drop);
                }
                ctx.changed = true;
                if is_decision_tool(&tool) {
                    let summary = decision_summary(&tool, input.tool_input.as_ref());
                    ctx.arm(Category::NeedsDecision, summary, "PreToolUse", Some(id));
                }
            }
        }
        "PostToolUse" | "PostToolUseFailure" => {
            let id = input.tool_use_id.clone().unwrap_or_default();
            let before = ctx.state.pending_tools.len();
            ctx.state.pending_tools.retain(|t| t.tool_use_id != id);
            ctx.changed |= before != ctx.state.pending_tools.len();
            ctx.close_if("tool_done", |e| match kind_of(e) {
                Kind::Bound => e.tool_use_id.as_deref() == Some(id.as_str()),
                Kind::Unbound => true,
                Kind::Elicitation => false,
            });
        }
        "PermissionRequest" => {
            let tool = input.tool_name.clone().unwrap_or_default();
            let hash = input_hash(input.tool_input.as_ref());
            let pending = &ctx.state.pending_tools;
            let bound = pending
                .iter()
                .rev()
                .find(|t| t.tool_name == tool && t.input_hash == hash)
                .or_else(|| pending.iter().rev().find(|t| t.tool_name == tool))
                .map(|t| t.tool_use_id.clone());
            let (category, summary) = if is_decision_tool(&tool) {
                (
                    Category::NeedsDecision,
                    decision_summary(&tool, input.tool_input.as_ref()),
                )
            } else {
                (
                    Category::NeedsPermission,
                    format!("wants to use {}", crate::sanitize::clean(&tool)),
                )
            };
            ctx.arm(category, summary, "PermissionRequest", bound);
        }
        "PermissionDenied" => {
            ctx.close_if("permission_denied", |e| kind_of(e) != Kind::Elicitation);
        }
        "Notification" => match input.notification_type.as_deref() {
            Some("permission_prompt") => {
                let now = ctx.now;
                if let Some(ep) = ctx.state.episode.as_mut().filter(|e| {
                    e.status.is_open()
                        && (kind_of(e) == Kind::Bound || e.category == Category::NeedsPermission)
                }) {
                    ep.confirmed_at = Some(now);
                    ctx.changed = true;
                }
            }
            Some("elicitation_dialog") => {
                let summary = crate::sanitize::clean(input.message.as_deref().unwrap_or(""));
                ctx.arm(Category::NeedsDecision, summary, ELICITATION_TRIGGER, None);
            }
            _ => {}
        },
        "ElicitationResult" => {
            ctx.close_if("elicitation_result", |e| kind_of(e) == Kind::Elicitation);
        }
        "Stop" => {
            ctx.clear_pending();
            ctx.close_if("stop", |e| kind_of(e) == Kind::Bound);
            let running = input
                .background_tasks
                .as_ref()
                .is_some_and(|b| b.iter().any(|t| t.status == "running"));
            if running {
                ctx.close_if("background_task", |e| kind_of(e) == Kind::Unbound);
                Entry::new(event)
                    .session(sid)
                    .kind("background_running")
                    .write();
            } else {
                let text = truncate_chars(input.last_assistant_message.as_deref().unwrap_or(""));
                let mut c = classify(text);
                if let Some(forced) = forced_category() {
                    c.category = forced;
                }
                ctx.arm(c.category, c.summary.clone(), "Stop", None);
                if cfg.training_collect {
                    crate::cli::training::collect(sid, text, &c);
                }
            }
        }
        "StopFailure" => {
            ctx.clear_pending();
            ctx.close_if("stop_failure", |e| kind_of(e) == Kind::Bound);
            ctx.arm(
                Category::Blocked,
                "stopped on an API error".into(),
                "StopFailure",
                None,
            );
        }
        _ => {}
    }
    if user_prompt && cfg.training_collect {
        crate::cli::training::record_reply(sid);
    }

    // A corrupt file is only replaced by an arming event; without one the
    // fresh in-memory state has no episode, so nothing is open either.
    let write = ctx.changed && (!corrupt || ctx.armed);
    let open = ctx.open_episode().is_some();
    if write {
        ctx.state.updated_at = now;
        if store.save(&ctx.state).is_err() {
            Entry::new(event).session(sid).kind("state_write").write();
            return out;
        }
    }
    drop(_lock);
    if open {
        // Any hook that leaves an open episode spawns a sweeper (R2); it
        // exits at once if one is already running.
        spawn_sweeper();
    }
    out
}

fn truncate_chars(s: &str) -> &str {
    match s.char_indices().nth(TEXT_MAX_CHARS) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn classify(text: &str) -> Classification {
    crate::classify::load().classify(text)
}

/// `TAP_TEST_FORCE_CATEGORY=<category>` (tests only, requires `TAP_DEV=1`).
fn forced_category() -> Option<Category> {
    if !config::dev() {
        return None;
    }
    let v = std::env::var("TAP_TEST_FORCE_CATEGORY").ok()?;
    serde_json::from_value(serde_json::Value::String(v)).ok()
}

/// The `SessionStart` warning when no ntfy topic is configured.
fn session_start_warning(event: &str, input: &HookInput) -> Option<String> {
    if event != "SessionStart" {
        return None;
    }
    let cwd = (!input.cwd.is_empty()).then(|| Path::new(input.cwd.as_str()));
    let loaded = config::load(cwd);
    if loaded.config.ntfy.topic.is_some() {
        return None;
    }
    Some(serde_json::json!({ "systemMessage": NO_TOPIC_WARNING }).to_string())
}

/// Spawn `tap sweep` detached: new session, stdio to `/dev/null`.
pub fn spawn_sweeper() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let Ok(exe) = std::env::current_exe() else {
        Entry::new("sweep").kind("no_exe").write();
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("sweep")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if cmd.spawn().is_err() {
        Entry::new("sweep").kind("spawn_failed").write();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_recursively() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"a":{"d":[{"z":1,"y":2}],"c":"é\n"}}"#).unwrap();
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"c":"é\n","d":[{"y":2,"z":1}]},"b":1}"#
        );
    }

    #[test]
    fn input_hash_is_order_independent() {
        let a: serde_json::Value = serde_json::from_str(r#"{"x":1,"y":"q"}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"y":"q","x":1}"#).unwrap();
        assert_eq!(input_hash(Some(&a)), input_hash(Some(&b)));
        assert_eq!(input_hash(Some(&a)).len(), 16);
        assert_ne!(input_hash(Some(&a)), input_hash(None));
    }

    #[test]
    fn decision_summaries() {
        let q = serde_json::json!({"questions":[{"question":"Keep the column?"}]});
        assert_eq!(
            decision_summary("AskUserQuestion", Some(&q)),
            "Keep the column?"
        );
        let p = serde_json::json!({"plan":"# Migrate DB\n\nsteps"});
        assert_eq!(
            decision_summary("ExitPlanMode", Some(&p)),
            "plan ready: Migrate DB"
        );
    }
}
