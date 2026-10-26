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
    "tap-on-shoulder: no ntfy topic configured, so no taps will be sent. Run /tap-on-shoulder:setup.";

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

/// Per-session lock wait. Well under Claude Code's 5 s hook timeout.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// Entry point. Returns what to print on stdout (usually nothing).
pub fn run(arg_event: &str) -> Option<String> {
    let event = event_name(arg_event).unwrap_or("unknown");
    let input = match read_input() {
        Ok(i) => i,
        Err(kind) => {
            Entry::new(event).kind(kind).write();
            return None;
        }
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

/// Read and parse stdin. Errors are log kinds: `oversize` or `bad_input`.
fn read_input() -> Result<HookInput, &'static str> {
    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .take(STDIN_MAX_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|_| "bad_input")?;
    if buf.len() > STDIN_MAX_BYTES {
        return Err("oversize");
    }
    serde_json::from_slice(&buf).map_err(|_| "bad_input")
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

/// What a bound episode is tied to (plan R3).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    /// One exact tool call.
    Id(String),
    /// Several pending calls of the same tool; any of them may be the one
    /// waiting on the prompt. Sorted.
    Candidates(Vec<String>),
    /// No pending call of the tool was known: bound by tool name.
    Name(String),
}

fn binding_of(ep: &Episode) -> Option<Binding> {
    if let Some(id) = &ep.tool_use_id {
        Some(Binding::Id(id.clone()))
    } else if !ep.candidate_ids.is_empty() {
        let mut c = ep.candidate_ids.clone();
        c.sort();
        Some(Binding::Candidates(c))
    } else {
        ep.bound_tool_name.clone().map(Binding::Name)
    }
}

fn set_binding(ep: &mut Episode, b: Option<Binding>) {
    ep.tool_use_id = None;
    ep.candidate_ids.clear();
    ep.bound_tool_name = None;
    match b {
        Some(Binding::Id(id)) => ep.tool_use_id = Some(id),
        Some(Binding::Candidates(c)) => ep.candidate_ids = c,
        Some(Binding::Name(n)) => ep.bound_tool_name = Some(n),
        None => {}
    }
}

/// The tool name a bound episode waits on, as far as it is known.
fn bound_tool(ep: &Episode, pending: &[PendingTool]) -> Option<String> {
    let name_of = |id: &str| {
        pending
            .iter()
            .find(|t| t.tool_use_id == id)
            .map(|t| t.tool_name.clone())
    };
    match binding_of(ep)? {
        Binding::Id(id) => name_of(&id),
        Binding::Candidates(c) => c.iter().find_map(|id| name_of(id)),
        Binding::Name(n) => Some(n),
    }
}

/// `PermissionRequest` binding: the pending call with the same name and
/// input hash; else the only pending call with that name; else all of them
/// (ambiguous); else the tool name alone.
fn bind_permission(pending: &[PendingTool], tool: &str, hash: &str) -> Binding {
    if let Some(t) = pending
        .iter()
        .rev()
        .find(|t| t.tool_name == tool && t.input_hash == hash)
    {
        return Binding::Id(t.tool_use_id.clone());
    }
    let mut same: Vec<String> = pending
        .iter()
        .filter(|t| t.tool_name == tool)
        .map(|t| t.tool_use_id.clone())
        .collect();
    match same.len() {
        0 => Binding::Name(tool.to_string()),
        1 => Binding::Id(same.remove(0)),
        _ => {
            same.sort();
            Binding::Candidates(same)
        }
    }
}

fn kind_of(ep: &Episode) -> Kind {
    if ep.trigger == ELICITATION_TRIGGER {
        Kind::Elicitation
    } else if binding_of(ep).is_some() {
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
    ///
    /// A bound re-arm whose binding differs from the current one is a new
    /// prompt (the previous one was answered), so its timers start fresh.
    fn arm(
        &mut self,
        category: Category,
        summary: String,
        trigger: &str,
        binding: Option<Binding>,
    ) {
        let bound = binding.is_some() || trigger == ELICITATION_TRIGGER;
        let now = self.now;
        let sid = self.state.session_id.clone();
        let transition;
        if let Some(ep) = self.state.episode.as_mut().filter(|e| e.status.is_open()) {
            let rebind = match (&binding, binding_of(ep)) {
                (Some(new), Some(cur)) => *new != cur,
                _ => false,
            };
            if rebind {
                ep.opened_at = now;
                ep.postponed_s = 0;
                ep.confirmed_at = None;
                ep.retry_at = None;
            }
            if bound || category.priority() > ep.category.priority() {
                ep.category = category;
                ep.summary = summary;
                ep.trigger = trigger.to_string();
                if bound {
                    set_binding(ep, binding);
                    ep.bound_at = Some(now);
                }
            }
            ep.due_at = ep.opened_at + self.cfg.grace_for(ep.category) as i64;
            transition = if rebind { "rebound" } else { "rearmed" };
        } else {
            let grace = self.cfg.grace_for(category) as i64;
            let mut ep = Episode {
                id: state::random_hex(8),
                status: EpisodeStatus::Open,
                category,
                summary,
                trigger: trigger.to_string(),
                tool_use_id: None,
                candidate_ids: Vec::new(),
                bound_tool_name: None,
                opened_at: now,
                bound_at: binding.as_ref().map(|_| now),
                due_at: now + grace,
                confirmed_at: None,
                attempts: 0,
                retry_at: None,
                sending_since: None,
                postponed_s: 0,
                closed_reason: None,
            };
            set_binding(&mut ep, binding);
            self.state.episode = Some(ep);
            // Detect Claude once per session (slow on macOS), and again only
            // when the recorded process is gone.
            let known = self
                .state
                .claude
                .as_ref()
                .is_some_and(crate::proc::same_process);
            if !known {
                self.state.claude = crate::proc::detect_claude();
            }
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
    let _lock = match store.lock_timeout(sid, LOCK_WAIT) {
        Ok(l) => l,
        Err(e) => {
            let kind = if e.kind() == std::io::ErrorKind::TimedOut {
                "lock_timeout"
            } else {
                "lock"
            };
            Entry::new(event).session(sid).kind(kind).write();
            return None;
        }
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
                    ctx.arm(
                        Category::NeedsDecision,
                        summary,
                        "PreToolUse",
                        Some(Binding::Id(id)),
                    );
                }
            }
        }
        "PostToolUse" | "PostToolUseFailure" => {
            let id = input.tool_use_id.clone().unwrap_or_default();
            let tool = input.tool_name.clone().unwrap_or_default();
            let before = ctx.state.pending_tools.len();
            ctx.state.pending_tools.retain(|t| t.tool_use_id != id);
            ctx.changed |= before != ctx.state.pending_tools.len();
            let pending = ctx.state.pending_tools.clone();
            ctx.close_if("tool_done", |e| match kind_of(e) {
                Kind::Bound => match binding_of(e) {
                    Some(Binding::Id(b)) => b == id,
                    // Only once no candidate is still pending.
                    Some(Binding::Candidates(c)) => !c
                        .iter()
                        .any(|x| pending.iter().any(|t| &t.tool_use_id == x)),
                    Some(Binding::Name(n)) => n == tool,
                    None => false,
                },
                Kind::Unbound => true,
                Kind::Elicitation => false,
            });
        }
        "PermissionRequest" => {
            let tool = input.tool_name.clone().unwrap_or_default();
            let hash = input_hash(input.tool_input.as_ref());
            let bound = bind_permission(&ctx.state.pending_tools, &tool, &hash);
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
            ctx.arm(category, summary, "PermissionRequest", Some(bound));
        }
        "PermissionDenied" => {
            // A bound episode closes only for its own tool: denying a
            // parallel call leaves the visible prompt waiting.
            let tool = input.tool_name.clone().unwrap_or_default();
            let pending = ctx.state.pending_tools.clone();
            ctx.close_if("permission_denied", |e| match kind_of(e) {
                Kind::Bound => bound_tool(e, &pending).is_some_and(|n| n == tool),
                Kind::Unbound => true,
                Kind::Elicitation => false,
            });
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
            let text = truncate_chars(input.last_assistant_message.as_deref().unwrap_or(""));
            let mut c = classify(text);
            if let Some(forced) = forced_category() {
                c.category = forced;
            }
            // With background work running, `done` and `info` mean "still
            // working" (a new Stop arrives when it finishes); a question or a
            // blocker still needs the user.
            let working = matches!(c.category, Category::Done | Category::Info);
            if running && working {
                ctx.close_if("background_task", |e| kind_of(e) == Kind::Unbound);
                Entry::new(event)
                    .session(sid)
                    .kind("background_running")
                    .write();
            } else {
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

/// `TAP_TEST_FORCE_CATEGORY=<category>` (debug builds with `TAP_DEV=1` only).
fn forced_category() -> Option<Category> {
    if !config::test_knobs() {
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
