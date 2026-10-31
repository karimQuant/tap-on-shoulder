//! Idle ledger scenarios (v0.2 plan part A, acceptance criteria 1–7 and 14).
//! Every test spawns the real `tap` binary with its own `TestEnv` and mock
//! ntfy, so tests are parallel-safe.

mod support;

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use support::*;

// ---------------------------------------------------------------- helpers

const QUESTION: &str =
    "I can keep the legacy column or drop it.\n\nShould I drop the legacy `old_email` column?";

/// The ledger is off unless the user config says `[stats] enabled = true`.
const STATS_ON: &str = "[stats]\nenabled = true\n";

fn env_for(url: &str) -> TestEnv {
    let mut env = TestEnv::new(url);
    env.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{url}\"\ntopic = \"tap-test-topic\"\n{STATS_ON}"
    ));
    env.set("TAP_TEST_CLAUDE_PID", "none");
    env
}

fn secs(n: f64) -> Duration {
    Duration::from_secs_f64(n)
}

fn hook(env: &TestEnv, input: &Value) {
    let out = env.hook(input);
    assert!(out.status.success(), "hook exit status: {:?}", out.status);
    assert!(out.stdout.is_empty(), "hook stdout must be empty");
}

fn status(env: &TestEnv, s: &str) -> String {
    env.state_json(s)
        .and_then(|v| v["episode"]["status"].as_str().map(String::from))
        .unwrap_or_default()
}

fn stats_dir(env: &TestEnv) -> PathBuf {
    env.data.path().join("stats")
}

fn ledger_path(env: &TestEnv) -> PathBuf {
    stats_dir(env).join("waits.jsonl")
}

fn ledger_text(env: &TestEnv) -> String {
    std::fs::read_to_string(ledger_path(env)).unwrap_or_default()
}

/// Every ledger line, parsed; panics on an invalid line.
fn lines(env: &TestEnv) -> Vec<Value> {
    ledger_text(env)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|_| panic!("invalid ledger line: {l}")))
        .collect()
}

fn mkdir_private(p: &std::path::Path) {
    std::fs::DirBuilder::new().mode(0o700).create(p).unwrap();
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn stats_json(env: &TestEnv, args: &[&str]) -> Value {
    let mut a = vec!["stats", "--json"];
    a.extend_from_slice(args);
    let out = env.run(&a);
    assert!(out.status.success(), "tap stats failed");
    serde_json::from_slice(&out.stdout).expect("stats json")
}

/// Ask a question, wait for the tap, return once the episode is `notified`.
fn tapped_question(env: &TestEnv, mock: &MockNtfy, s: &str, n: usize) {
    hook(env, &stop(s, QUESTION));
    assert_eq!(mock.wait_for(n, secs(6.0)), n, "tap not sent");
    assert!(eventually(secs(3.0), || status(env, s) == "notified"));
}

/// A process standing in for Claude; killed (whole group) on drop.
struct FakeClaude(Child);

impl FakeClaude {
    fn idle() -> Self {
        let child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        FakeClaude(child)
    }

    fn pid(&self) -> String {
        self.0.id().to_string()
    }

    fn kill(&mut self) {
        // SAFETY: plain syscall on our own process group.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

impl Drop for FakeClaude {
    fn drop(&mut self) {
        self.kill();
    }
}

fn line(opened: i64, closed: i64, notified: Option<i64>, reason: &str, user: bool) -> Value {
    let mut v = json!({"v": 1, "opened": opened, "closed": closed, "category": "needs_decision",
                       "reason": reason, "user": user});
    if let Some(n) = notified {
        v["notified"] = json!(n);
    }
    v
}

fn with_project(mut v: Value, p: &str) -> Value {
    v["project"] = json!(p);
    v
}

// ---------------------------------------------------------------- scenarios

#[test]
fn s13_reply_after_tap_one_line() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    tapped_question(&env, &mock, "s13", 1);
    let notified_at = env.state_json("s13").unwrap()["episode"]["notified_at"]
        .as_i64()
        .expect("notified_at set on a successful send");

    hook(&env, &user_prompt("s13"));
    let ep = env.state_json("s13").unwrap()["episode"].clone();
    assert_eq!(ep["status"], "notified", "tap state is unchanged");
    assert_eq!(ep["finished_reason"], "user_prompt");

    // A second reply and a later session end never record it again.
    hook(&env, &user_prompt("s13"));
    hook(&env, &session_end("s13"));

    let l = lines(&env);
    assert_eq!(l.len(), 1, "{l:?}");
    let x = &l[0];
    assert_eq!(x["v"], 1);
    assert_eq!(x["reason"], "user_prompt");
    assert_eq!(x["user"], true);
    assert_eq!(x["category"], "needs_decision");
    assert_eq!(x["notified"].as_i64(), Some(notified_at));
    assert_eq!(x["project"], "payments-service");
    assert!(x["closed"].as_i64().unwrap() >= x["opened"].as_i64().unwrap());
    assert!(x.get("early").is_none());

    let text = ledger_text(&env);
    for frag in ["legacy", "old_email", "column", "PROMPT_TEXT"] {
        assert!(!text.contains(frag), "message text in the ledger: {frag}");
    }
    assert!(!env.log_text().contains("legacy"));
    let mode = std::fs::metadata(ledger_path(&env))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let dmode = std::fs::metadata(stats_dir(&env))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dmode & 0o777, 0o700);
    assert_eq!(mock.count(), 1);
}

#[test]
fn s14_rearm_one_line_rebind_two() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "1h");

    // Re-arm (same binding): one episode, one line.
    let ask = ask_user_question_input("Which region?");
    hook(&env, &pre_tool("s14", "AskUserQuestion", "q1", ask.clone()));
    hook(
        &env,
        &permission_request("s14", "AskUserQuestion", ask.clone()),
    );
    hook(&env, &post_tool("s14", "AskUserQuestion", "q1", ask));
    let l = lines(&env);
    assert_eq!(l.len(), 1, "{l:?}");
    assert_eq!(l[0]["reason"], "tool_done");
    assert_eq!(l[0]["category"], "needs_decision");

    // Rebind: the previous prompt was answered (one line), then the new one.
    let a = json!({"command": "git push"});
    let b = json!({"command": "rm -rf build"});
    hook(&env, &pre_tool("s14", "Bash", "t1", a.clone()));
    hook(&env, &pre_tool("s14", "Bash", "t2", b.clone()));
    hook(&env, &permission_request("s14", "Bash", a.clone()));
    hook(&env, &permission_request("s14", "Bash", b.clone()));
    assert_eq!(lines(&env).len(), 2);
    hook(&env, &post_tool("s14", "Bash", "t1", a));
    hook(&env, &post_tool("s14", "Bash", "t2", b));
    let l = lines(&env);
    assert_eq!(l.len(), 3, "{l:?}");
    assert_eq!(l[1]["reason"], "rebind");
    assert_eq!(l[1]["user"], true);
    assert_eq!(l[1]["category"], "needs_permission");
    assert!(l[1].get("notified").is_none());
    assert_eq!(l[2]["reason"], "tool_done");
    assert_eq!(l[2]["category"], "needs_permission");
    assert!(!ledger_text(&env).contains("git push"));
    assert!(!ledger_text(&env).contains("Bash"), "no tool names");
    assert!(!ledger_text(&env).contains("region"));
    assert_eq!(mock.count(), 0);
}

#[test]
fn s15_stats_privacy_minimal_omits_project() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    env.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\nprivacy = \"category-only\"\n{STATS_ON}",
        mock.url
    ));
    tapped_question(&env, &mock, "s15", 1);
    hook(&env, &user_prompt("s15"));
    let l = lines(&env);
    assert_eq!(l.len(), 1);
    assert!(l[0].get("project").is_none(), "{}", l[0]);
    assert!(!ledger_text(&env).contains("payments"));
}

#[test]
fn s16_stats_disabled_writes_nothing() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    env.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\n[stats]\nenabled = false\n",
        mock.url
    ));
    tapped_question(&env, &mock, "s16", 1);
    hook(&env, &user_prompt("s16"));
    hook(&env, &stop("s16b", QUESTION));
    hook(&env, &session_end("s16b"));
    assert!(
        !stats_dir(&env).exists(),
        "stats dir created while disabled"
    );

    let d = env.run(&["doctor"]);
    let dtext = String::from_utf8_lossy(&d.stdout);
    assert!(dtext.contains("stats: idle ledger off"), "{dtext}");
    assert!(dtext.contains("[stats] enabled = true"), "{dtext}");

    // A project file cannot turn it back on.
    let proj = TempDir::new("tap-proj");
    std::fs::create_dir_all(proj.path().join(".claude")).unwrap();
    std::fs::write(
        proj.path().join(".claude/tap-on-shoulder.toml"),
        "[stats]\nenabled = true\n",
    )
    .unwrap();
    let cwd = proj.path().to_str().unwrap();
    let mut s = stop("s16c", QUESTION);
    s["cwd"] = json!(cwd);
    hook(&env, &s);
    let mut u = user_prompt("s16c");
    u["cwd"] = json!(cwd);
    hook(&env, &u);
    assert!(!stats_dir(&env).exists());
    assert!(env.log_text().contains("project_key_ignored"));
}

#[test]
fn s16b_stats_missing_key_means_off() {
    // An upgrader's config has no `[stats]` table: nothing is recorded.
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = TestEnv::new(&mock.url);
    env.set("TAP_TEST_CLAUDE_PID", "none");
    tapped_question(&env, &mock, "s16m", 1);
    hook(&env, &user_prompt("s16m"));
    assert!(!stats_dir(&env).exists(), "ledger written without consent");

    let d = env.run(&["doctor"]);
    let dtext = String::from_utf8_lossy(&d.stdout);
    assert!(dtext.contains("stats: idle ledger off"), "{dtext}");
    assert!(
        dtext.contains("turn on with `[stats] enabled = true`"),
        "{dtext}"
    );

    let s = env.run(&["stats"]);
    let stext = String::from_utf8_lossy(&s.stdout);
    assert!(s.status.success());
    assert!(stext.contains("ledger: off"), "{stext}");
    assert!(stext.contains("[stats] enabled = true"), "{stext}");
}

/// Tap + reply, then a second question that must still tap, with the ledger
/// write failing every time.
fn assert_failure_still_taps(env: &TestEnv, mock: &MockNtfy, s: &str) {
    hook(env, &stop(s, QUESTION));
    let t = Instant::now();
    hook(env, &user_prompt(s));
    assert!(t.elapsed() < secs(1.5), "stats failure delayed a hook");
    tapped_question(env, mock, s, 1);
    hook(env, &user_prompt(s));
    assert_eq!(
        env.state_json(s).unwrap()["episode"]["finished_reason"],
        "user_prompt"
    );
    assert!(env.log_text().contains("\"kind\":\"stats_write_failed\""));
}

#[test]
fn s17_stats_write_failure_still_taps() {
    // Read-only stats directory.
    {
        let mock = MockNtfy::start(Mode::Ok);
        let env = env_for(&mock.url);
        let dir = stats_dir(&env);
        mkdir_private(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_failure_still_taps(&env, &mock, "s17a");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!ledger_path(&env).exists());
    }
    // Symlinked ledger file.
    {
        let mock = MockNtfy::start(Mode::Ok);
        let env = env_for(&mock.url);
        let dir = stats_dir(&env);
        mkdir_private(&dir);
        let target = env.data.path().join("elsewhere.txt");
        std::fs::write(&target, "untouched\n").unwrap();
        std::os::unix::fs::symlink(&target, ledger_path(&env)).unwrap();
        assert_failure_still_taps(&env, &mock, "s17b");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched\n");
    }
    // Lock held by another process.
    {
        let mock = MockNtfy::start(Mode::Ok);
        let env = env_for(&mock.url);
        let dir = stats_dir(&env);
        mkdir_private(&dir);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("waits.lock"))
            .unwrap();
        // SAFETY: fd is valid for the lifetime of `lock`.
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_failure_still_taps(&env, &mock, "s17c");
        assert_eq!(ledger_text(&env), "");
        drop(lock);
    }
}

#[test]
fn s18_concurrent_appends_and_rotation_valid_jsonl() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "1h");
    let dir = stats_dir(&env);
    mkdir_private(&dir);

    // Over 1 MiB of lines older than 90 days, plus a few recent ones.
    let t = now();
    let mut seed = String::new();
    let old = with_project(
        line(
            t - 101 * 86_400,
            t - 100 * 86_400,
            None,
            "user_prompt",
            true,
        ),
        "old-project-padding-padding-padding",
    )
    .to_string();
    while seed.len() < (1 << 20) + 4096 {
        seed.push_str(&old);
        seed.push('\n');
    }
    let recent = 5;
    for i in 0..recent {
        let v = with_project(
            line(t - 86_400 - 60, t - 86_400 + i, None, "tool_done", true),
            "recent",
        );
        seed.push_str(&v.to_string());
        seed.push('\n');
    }
    std::fs::write(ledger_path(&env), &seed).unwrap();

    // Sweeper close (session_gone) running alongside the hooks.
    let mut claude = FakeClaude::idle();
    let mut gone = env.command(&["hook", "Stop"]);
    gone.env("TAP_TEST_CLAUDE_PID", claude.pid())
        .env("TAP_GRACE_OVERRIDE", "1s")
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut child = gone.spawn().unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(stop("s18gone", QUESTION).to_string().as_bytes())
            .unwrap();
    }
    assert!(child.wait().unwrap().success());
    claude.kill();

    // Concurrent hook appends from many sessions.
    let n = 12;
    std::thread::scope(|sc| {
        for i in 0..n {
            let env = &env;
            sc.spawn(move || {
                let s = format!("s18-{i}");
                hook(env, &stop(&s, QUESTION));
                hook(env, &user_prompt(&s));
            });
        }
    });
    assert!(eventually(secs(8.0), || env.state_json("s18gone").unwrap()
        ["episode"]["closed_reason"]
        == "session_gone"));
    // The sweeper records right after its close.
    std::thread::sleep(secs(0.5));

    let l = lines(&env); // every line parses
    let text = ledger_text(&env);
    assert!(text.ends_with('\n'));
    assert!(text.len() < 1 << 20, "rotated");
    assert!(
        !text.contains("old-project"),
        "lines older than 90 days dropped"
    );
    let kept = l.iter().filter(|x| x["project"] == "recent").count();
    assert_eq!(kept, recent as usize, "recent lines kept");
    let new = l.len() - kept;
    let failed = env.log_text().matches("stats_write_failed").count();
    assert_eq!(
        new + failed,
        n + 1,
        "every wait is either recorded or logged as failed"
    );
    assert!(new >= 1);
    let mode = std::fs::metadata(ledger_path(&env))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(mock.count(), 0);
}

#[test]
fn s19_stats_report() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    mkdir_private(&stats_dir(&env));
    let t = now() - 60;
    let h = 3600;
    let fixture = [
        // 600 s, replied 120 s after the tap.
        with_project(line(t - h - 600, t - h, Some(t - h - 120), "user_prompt", true), "alpha")
            .to_string(),
        // 3 h, capped at 2 h; replied 300 s after the tap.
        with_project(line(t - 4 * h, t - h, Some(t - h - 300), "tool_done", true), "beta")
            .to_string(),
        // Negative duration, clamped to 0.
        with_project(line(t + 50, t, None, "permission_denied", true), "alpha").to_string(),
        // Outside the 7-day window by `closed` (opened inside would not matter).
        with_project(line(t - 8 * 86_400 - 1000, t - 8 * 86_400, None, "rebind", true), "old")
            .to_string(),
        // Not a user action: excluded from the headline.
        line(t - 500, t - 10, Some(t - 100), "session_end", false).to_string(),
        // 60 s, replied 60 s after the tap.
        with_project(line(t - 60, t, Some(t - 60), "elicitation_result", true), "gamma")
            .to_string(),
        // 30 s; fourth project, cut from the top 3.
        with_project(line(t - 30, t, None, "user_prompt", true), "delta").to_string(),
        // Corrupt and unknown lines.
        "not json at all".to_string(),
        r#"{"v":2,"opened":1,"closed":2,"category":"blocked","reason":"user_prompt","user":true}"#
            .to_string(),
        line(t - 10, t, None, "bogus_reason", true).to_string(),
        r#"{"v":1,"opened":1,"closed":2,"category":"done","reason":"user_prompt","user":true}"#
            .to_string(),
        r#"{"v":1,"opened":1,"closed":2,"category":"needs_decision","reason":"session_end","user":true}"#
            .to_string(),
        "{\"v\":1,\"opened\":".to_string(),
    ];
    std::fs::write(ledger_path(&env), fixture.join("\n") + "\n").unwrap();

    let r = stats_json(&env, &[]);
    assert_eq!(
        r,
        json!({
            "days": 7,
            "blocked_s": 600 + 7200 + 60 + 30,
            "waits": 5,
            "tapped": 3,
            "median_reply_min": 2.0,
            "top_projects": [
                {"project": "beta", "blocked_s": 7200, "waits": 1},
                {"project": "alpha", "blocked_s": 600, "waits": 2},
                {"project": "gamma", "blocked_s": 60, "waits": 1},
            ],
            "not_user": 1,
            "skipped": 6,
        })
    );

    let r = stats_json(&env, &["--days", "9"]);
    assert_eq!(r["waits"], 6);
    assert_eq!(r["blocked_s"], 600 + 7200 + 60 + 30 + 1000);
    assert_eq!(r["median_reply_min"], 2.0);

    let out = env.run(&["stats"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        text.contains("blocked on you for 2 h 11 m across 5 waits"),
        "{text}"
    );
    assert!(text.contains("skipped: 6"), "{text}");

    assert_eq!(env.run(&["stats", "--days", "0"]).status.code(), Some(2));
    assert_eq!(env.run(&["stats", "--bogus"]).status.code(), Some(2));

    // Empty or missing ledger.
    let empty = env_for(&mock.url);
    let r = stats_json(&empty, &[]);
    assert_eq!(r["waits"], 0);
    assert_eq!(r["blocked_s"], 0);
    assert!(r["median_reply_min"].is_null());
}

#[test]
fn s19b_non_user_closes_excluded_from_headline() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "1h");

    // session_end
    hook(&env, &stop("s19e", QUESTION));
    hook(&env, &session_end("s19e"));
    // session_start (resume)
    hook(&env, &stop("s19s", QUESTION));
    let mut start = session_start("s19s");
    start["source"] = json!("resume");
    hook(&env, &start);
    // session_gone (sweeper)
    let mut claude = FakeClaude::idle();
    let mut genv = env_for(&mock.url);
    genv.set("TAP_GRACE_OVERRIDE", "1s");
    genv.set("TAP_TEST_CLAUDE_PID", &claude.pid());
    // Same data and config dirs as `env`.
    let mut cmd = genv.command(&["hook", "Stop"]);
    cmd.env("CLAUDE_PLUGIN_DATA", env.data.path())
        .env("HOME", env.config_home.path())
        .env("XDG_CONFIG_HOME", env.config_home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stop("s19g", QUESTION).to_string().as_bytes())
            .unwrap();
    }
    assert!(child.wait().unwrap().success());
    claude.kill();
    assert!(eventually(secs(8.0), || ledger_text(&env).contains("session_gone")));

    let l = lines(&env);
    let mut reasons: Vec<&str> = l.iter().map(|x| x["reason"].as_str().unwrap()).collect();
    reasons.sort();
    assert_eq!(reasons, ["session_end", "session_gone", "session_start"]);
    assert!(l.iter().all(|x| x["user"] == false), "{l:?}");

    let r = stats_json(&env, &[]);
    assert_eq!(r["waits"], 0);
    assert_eq!(r["blocked_s"], 0);
    assert_eq!(r["not_user"], 3);
    assert_eq!(mock.count(), 0);
}

#[test]
fn ledger_setup_and_doctor_mention_it() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    std::fs::remove_file(env.config_dir().join("config.toml")).unwrap();
    let s = env.run(&["setup", "--server", &mock.url]);
    let out = String::from_utf8_lossy(&s.stdout);
    assert!(s.status.success());
    assert!(out.contains("idle ledger: on"), "{out}");
    assert!(out.contains("stats/waits.jsonl"), "{out}");
    assert!(out.contains("[stats] enabled = false"), "{out}");

    let d = env.run(&["doctor"]);
    let dtext = String::from_utf8_lossy(&d.stdout);
    assert!(dtext.contains("stats: idle ledger on"), "{dtext}");
    assert!(dtext.contains("waits.jsonl (0 line(s)"), "{dtext}");
    assert!(dtext.contains("[stats] enabled = false"), "{dtext}");
}
