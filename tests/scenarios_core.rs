//! Core scenarios (plan acceptance criterion 1 and R8). Every test spawns the
//! real `tap` binary with its own `TestEnv` and mock ntfy, so tests are
//! parallel-safe.

mod support;

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use support::*;

// ---------------------------------------------------------------- helpers

/// A `TestEnv` with Claude detection disabled (no liveness/process checks).
fn env_for(url: &str) -> TestEnv {
    let mut env = TestEnv::new(url);
    env.set("TAP_TEST_CLAUDE_PID", "none");
    env
}

fn assert_quiet(out: &Output) {
    assert!(out.status.success(), "hook exit status: {:?}", out.status);
    assert!(
        out.stdout.is_empty(),
        "hook stdout must be empty, got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn hook(env: &TestEnv, input: &Value) {
    assert_quiet(&env.hook(input));
}

fn episode(env: &TestEnv, s: &str) -> Value {
    env.state_json(s)
        .map(|v| v["episode"].clone())
        .unwrap_or(Value::Null)
}

fn status(env: &TestEnv, s: &str) -> String {
    episode(env, s)["status"].as_str().unwrap_or("").to_string()
}

fn with_cwd(mut v: Value, cwd: &str) -> Value {
    v["cwd"] = json!(cwd);
    v
}

fn secs(n: f64) -> Duration {
    Duration::from_secs_f64(n)
}

/// Wait `d`, then assert the mock saw exactly `n` requests.
fn settle(mock: &MockNtfy, d: Duration, n: usize) {
    std::thread::sleep(d);
    assert_eq!(mock.count(), n, "unexpected request count");
}

/// A process standing in for Claude; killed (whole group) on drop.
struct FakeClaude {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl FakeClaude {
    /// Idle: no children.
    fn idle() -> Self {
        let child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        FakeClaude { child, stdin: None }
    }

    /// Spawns a long-running child ("approved tool") when `start_tool` is
    /// called. The trailing `echo` keeps `sh` from exec'ing `sleep`.
    fn with_tool() -> Self {
        let mut child = Command::new("sh")
            .args(["-c", "read x; sleep 30; echo done"])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn sh");
        let stdin = child.stdin.take();
        FakeClaude { child, stdin }
    }

    fn pid(&self) -> String {
        self.child.id().to_string()
    }

    fn start_tool(&mut self) {
        let mut s = self.stdin.take().expect("stdin");
        writeln!(s, "go").unwrap();
        drop(s);
        // Wait for the tool process to appear.
        let pid = self.child.id();
        assert!(eventually(secs(3.0), || {
            std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                .map(|c| !c.trim().is_empty())
                .unwrap_or(true)
        }));
    }

    fn kill(&mut self) {
        // SAFETY: plain syscall on our own process group.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

impl Drop for FakeClaude {
    fn drop(&mut self) {
        self.kill();
    }
}

const QUESTION: &str =
    "I can keep the legacy column or drop it.\n\nShould I drop the legacy `old_email` column?";

// ---------------------------------------------------------------- scenarios

#[test]
fn s01_question_taps_once() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    hook(&env, &stop("s01", QUESTION));
    assert_eq!(status(&env, "s01"), "open");
    assert_eq!(mock.wait_for(1, secs(5.0)), 1);
    settle(&mock, secs(2.0), 1);

    let req = &mock.requests()[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/");
    let body = req.json();
    assert_eq!(body["topic"], "tap-test-topic");
    let title = body["title"].as_str().unwrap();
    assert!(title.starts_with("payments-service · waiting "), "{title}");
    let msg = body["message"].as_str().unwrap();
    assert!(msg.contains("drop the legacy"), "{msg}");
    assert!(msg.contains(": "), "{msg}");
    assert!(body["priority"].as_u64().unwrap() >= 2);
    for (k, v) in &req.headers {
        assert!(!v.contains("legacy"), "user text in header {k}");
        assert_ne!(k, "authorization");
    }
    assert!(eventually(secs(2.0), || status(&env, "s01") == "notified"));
}

#[test]
fn s02_reply_cancels() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "3s");
    hook(&env, &stop("s02", QUESTION));
    hook(&env, &user_prompt("s02"));
    let ep = episode(&env, "s02");
    assert_eq!(ep["status"], "closed");
    assert_eq!(ep["closed_reason"], "user_prompt");
    settle(&mock, secs(4.5), 0);
    assert!(!env.log_text().contains("PROMPT_TEXT"));
}

#[test]
fn s03_resume_cancels_stale() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "3s");

    // Claude resumes (tool activity) before the grace period ends.
    hook(&env, &stop("s03", "Old turn: which option should I take?"));
    let first = episode(&env, "s03")["id"].clone();
    let bash = json!({"command": "cargo build"});
    hook(&env, &pre_tool("s03", "Bash", "t1", bash.clone()));
    assert_eq!(episode(&env, "s03")["closed_reason"], "activity");
    hook(&env, &post_tool("s03", "Bash", "t1", bash));
    hook(&env, &stop("s03", "New turn: deploy to staging or prod?"));
    let second = episode(&env, "s03")["id"].clone();
    assert_ne!(first, second, "a newer turn gets a new episode id");

    // A resumed session (SessionStart) closes a stale episode too.
    hook(&env, &stop("s03r", "Stale: shall I continue?"));
    hook(
        &env,
        &with(session_start("s03r"), json!({"source": "resume"})),
    );
    assert_eq!(episode(&env, "s03r")["closed_reason"], "session_start");

    assert_eq!(mock.wait_for(1, secs(6.0)), 1);
    settle(&mock, secs(1.5), 1);
    let msg = mock.requests()[0].json()["message"].to_string();
    assert!(msg.contains("New turn"), "{msg}");
}

fn with(mut v: Value, extra: Value) -> Value {
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v
}

#[test]
fn s04_silent_tool_no_tap() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    hook(
        &env,
        &pre_tool("s04", "Bash", "t1", json!({"command": "sleep 600"})),
    );
    assert!(episode(&env, "s04").is_null());
    assert_eq!(
        env.state_json("s04").unwrap()["pending_tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Background work still running: Stop opens nothing.
    hook(
        &env,
        &stop_with_bg(
            "s04b",
            "Started the build in the background. Want me to wait?",
        ),
    );
    assert!(episode(&env, "s04b").is_null());
    settle(&mock, secs(3.0), 0);
}

#[test]
fn s05_completion_pref() {
    // (a) default: completion taps are off.
    let mock_a = MockNtfy::start(Mode::Ok);
    let mut a = env_for(&mock_a.url);
    a.set("TAP_TEST_FORCE_CATEGORY", "done");

    // (b) user config enables completion taps.
    let mock_b = MockNtfy::start(Mode::Ok);
    let mut b = env_for(&mock_b.url);
    b.set("TAP_TEST_FORCE_CATEGORY", "done");
    b.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\n[alerts]\ncompletion = true\n",
        mock_b.url
    ));

    // (c) a project config may enable completion taps.
    let mock_c = MockNtfy::start(Mode::Ok);
    let mut c = env_for(&mock_c.url);
    c.set("TAP_TEST_FORCE_CATEGORY", "done");
    let proj = TempDir::new("tap-proj");
    std::fs::create_dir_all(proj.path().join(".claude")).unwrap();
    std::fs::write(
        proj.path().join(".claude/tap-on-shoulder.toml"),
        "[alerts]\ncompletion = true\n",
    )
    .unwrap();
    let proj_cwd = proj.path().to_str().unwrap().to_string();

    let done = "All tests pass ✅. Merged the fix.";
    hook(&a, &stop("s05", done));
    hook(&b, &stop("s05", done));
    hook(&c, &with_cwd(stop("s05", done), &proj_cwd));

    let ep = episode(&a, "s05");
    assert_eq!(ep["category"], "done");
    assert_eq!(ep["status"], "closed");
    assert_eq!(ep["closed_reason"], "suppressed");

    assert_eq!(mock_b.wait_for(1, secs(5.0)), 1);
    assert_eq!(mock_c.wait_for(1, secs(5.0)), 1);
    let body = mock_b.requests()[0].json();
    assert_eq!(body["priority"], 2);
    assert!(body["message"].as_str().unwrap().starts_with("finished"));
    settle(&mock_a, secs(0.5), 0);
}

#[test]
fn s06_permission_policy() {
    let claude = FakeClaude::idle();
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_TEST_CLAUDE_PID", &claude.pid());

    let input = json!({"command": "rm -rf build", "description": "clean"});
    hook(&env, &pre_tool("s06", "Bash", "t1", input.clone()));
    hook(&env, &permission_request("s06", "Bash", input));
    let ep = episode(&env, "s06");
    assert_eq!(ep["category"], "needs_permission");
    assert_eq!(ep["tool_use_id"], "t1");
    assert_eq!(ep["summary"], "wants to use Bash");
    assert_eq!(
        env.state_json("s06").unwrap()["claude"]["pid"].to_string(),
        claude.pid()
    );

    // The permission_prompt notification only confirms.
    hook(&env, &permission_prompt("s06"));
    let ep2 = episode(&env, "s06");
    assert_eq!(ep2["id"], ep["id"]);
    assert!(ep2["confirmed_at"].is_i64());

    // A notification without an episode never opens one.
    hook(&env, &permission_prompt("s06n"));
    assert!(episode(&env, "s06n").is_null());

    // Approved and finished before the grace period: no tap.
    let mut quick = env_for(&mock.url);
    quick.set("TAP_GRACE_OVERRIDE", "4s");
    let ls = json!({"command": "ls"});
    hook(&quick, &pre_tool("s06q", "Bash", "q1", ls.clone()));
    hook(&quick, &permission_request("s06q", "Bash", ls.clone()));
    hook(&quick, &post_tool("s06q", "Bash", "q1", ls));
    assert_eq!(episode(&quick, "s06q")["closed_reason"], "tool_done");

    // AskUserQuestion opens needs_decision bound to its id.
    let ask = ask_user_question_input("Which database should we use?");
    hook(
        &quick,
        &pre_tool("s06a", "AskUserQuestion", "a1", ask.clone()),
    );
    let ep = episode(&quick, "s06a");
    assert_eq!(ep["category"], "needs_decision");
    assert_eq!(ep["summary"], "Which database should we use?");
    hook(&quick, &post_tool("s06a", "AskUserQuestion", "a1", ask));
    assert_eq!(status(&quick, "s06a"), "closed");

    assert_eq!(mock.wait_for(1, secs(6.0)), 1);
    let body = mock.requests()[0].json();
    assert_eq!(
        body["message"],
        "waiting on a permission prompt: wants to use Bash"
    );
    assert_eq!(body["priority"], 4);
    settle(&mock, secs(4.0), 1);
}

#[test]
fn s06b_approved_long_tool_no_tap() {
    let mut claude = FakeClaude::with_tool();
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_TEST_CLAUDE_PID", &claude.pid());
    env.set("TAP_DEV_FAST", "1");
    // 2 s grace: the tool must start before the first check.
    env.set("TAP_GRACE_OVERRIDE", "2s");

    let input = json!({"command": "make test"});
    hook(&env, &pre_tool("s06b", "Bash", "t1", input.clone()));
    hook(&env, &permission_request("s06b", "Bash", input.clone()));
    // The user approves; the tool now runs as a child of Claude.
    claude.start_tool();

    assert!(
        eventually(secs(5.0), || episode(&env, "s06b")["postponed_s"]
            .as_i64()
            .unwrap_or(0)
            >= 1),
        "permission tap must be postponed while the tool runs: {}",
        env.log_text()
    );
    assert_eq!(mock.count(), 0);
    hook(&env, &post_tool("s06b", "Bash", "t1", input));
    assert_eq!(status(&env, "s06b"), "closed");
    settle(&mock, secs(1.5), 0);
}

#[test]
fn s06c_parallel_tool_does_not_close_permission() {
    let claude = FakeClaude::idle();
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_TEST_CLAUDE_PID", &claude.pid());
    env.set("TAP_GRACE_OVERRIDE", "3s");

    let a = json!({"command": "git push"});
    let b = json!({"command": "git status"});
    hook(&env, &pre_tool("s06c", "Bash", "ta", a.clone()));
    hook(&env, &pre_tool("s06c", "Bash", "tb", b.clone()));
    hook(
        &env,
        &pre_tool("s06c", "Read", "tc", json!({"file_path": "x"})),
    );
    // Same tool name, binds by input hash (not the most recent Bash).
    hook(&env, &permission_request("s06c", "Bash", a));
    assert_eq!(episode(&env, "s06c")["tool_use_id"], "ta");
    // Parallel tools finishing do not close the permission episode.
    hook(&env, &post_tool("s06c", "Bash", "tb", b));
    hook(
        &env,
        &post_tool("s06c", "Read", "tc", json!({"file_path": "x"})),
    );
    hook(
        &env,
        &pre_tool("s06c", "Grep", "td", json!({"pattern": "x"})),
    );
    assert_eq!(status(&env, "s06c"), "open");

    // No hash match: falls back to the most recent pending entry of that name.
    hook(
        &env,
        &pre_tool("s06f", "Bash", "f1", json!({"command": "a"})),
    );
    hook(
        &env,
        &pre_tool("s06f", "Bash", "f2", json!({"command": "b"})),
    );
    hook(
        &env,
        &permission_request("s06f", "Bash", json!({"command": "other"})),
    );
    assert_eq!(episode(&env, "s06f")["tool_use_id"], "f2");
    hook(
        &env,
        &post_tool("s06f", "Bash", "f2", json!({"command": "b"})),
    );
    assert_eq!(status(&env, "s06f"), "closed");

    assert_eq!(mock.wait_for(1, secs(7.0)), 1);
    settle(&mock, secs(1.0), 1);
    assert_eq!(status(&env, "s06c"), "notified");
}

#[test]
fn s06d_permission_postpone_cap() {
    let mut claude = FakeClaude::with_tool();
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_TEST_CLAUDE_PID", &claude.pid());
    env.set("TAP_DEV_FAST", "1");
    // 2 s grace: the tool must start before the first check.
    env.set("TAP_GRACE_OVERRIDE", "2s");

    let input = json!({"command": "long job"});
    hook(&env, &pre_tool("s06d", "Bash", "t1", input.clone()));
    hook(&env, &permission_request("s06d", "Bash", input));
    claude.start_tool();
    // The tool keeps running past the postponement cap: the tap goes out.
    assert_eq!(mock.wait_for(1, secs(15.0)), 1);
    let ep = episode(&env, "s06d");
    assert!(ep["postponed_s"].as_i64().unwrap() >= 3, "{ep}");
    settle(&mock, secs(1.0), 1);
}

#[test]
fn s07_two_sessions() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    hook(
        &env,
        &with_cwd(stop("s07a", "Alpha: ship it now?"), "/work/alpha"),
    );
    hook(
        &env,
        &with_cwd(stop("s07b", "Beta: which branch?"), "/work/beta"),
    );
    assert_eq!(mock.wait_for(2, secs(6.0)), 2);
    settle(&mock, secs(1.5), 2);
    let mut titles: Vec<String> = mock
        .requests()
        .iter()
        .map(|r| r.json()["title"].as_str().unwrap().to_string())
        .collect();
    titles.sort();
    assert!(titles[0].starts_with("alpha · "), "{titles:?}");
    assert!(titles[1].starts_with("beta · "), "{titles:?}");
}

#[test]
fn s08_restart_no_duplicate() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    hook(&env, &stop("s08", QUESTION));
    assert_eq!(mock.wait_for(1, secs(5.0)), 1);
    assert!(eventually(secs(2.0), || status(&env, "s08") == "notified"));

    // A restarted sweeper and further non-arming hooks never resend.
    assert!(env.run(&["sweep"]).status.success());
    hook(&env, &permission_prompt("s08"));
    hook(
        &env,
        &with(session_start("s08"), json!({"source": "resume"})),
    );
    assert_eq!(status(&env, "s08"), "notified");

    // A sweeper that died mid-send leaves `sending`; after 2 minutes any
    // sweeper turns it into `sent_uncertain` without sending again.
    let mut crashed = env.state_json("s08").unwrap();
    crashed["session_id"] = json!("s08x");
    crashed["episode"]["id"] = json!("00000000deadbeef");
    crashed["episode"]["status"] = json!("sending");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    crashed["episode"]["sending_since"] = json!(now - 200);
    std::fs::write(env.state_dir().join("s08x.json"), crashed.to_string()).unwrap();
    assert!(eventually(secs(8.0), || {
        env.run(&["sweep"]);
        status(&env, "s08x") == "sent_uncertain"
    }));
    settle(&mock, secs(1.0), 1);
}

#[test]
fn s08b_session_gone_no_tap() {
    let mut claude = FakeClaude::idle();
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_TEST_CLAUDE_PID", &claude.pid());
    env.set("TAP_GRACE_OVERRIDE", "2s");
    hook(&env, &stop("s08b", QUESTION));
    assert_eq!(
        env.state_json("s08b").unwrap()["claude"]["pid"].to_string(),
        claude.pid()
    );
    claude.kill();
    assert!(eventually(secs(5.0), || episode(&env, "s08b")
        ["closed_reason"]
        == "session_gone"));
    settle(&mock, secs(1.0), 0);
}

#[test]
fn s09b_ntfy_unreachable_failed() {
    // Connection refused: 3 attempts with backoff, then `failed`.
    let mut refused = env_for(&MockNtfy::refused_url());
    refused.set("TAP_DEV_FAST", "1");
    hook(&refused, &stop("s09b", QUESTION));

    // HTTP 500: delivered but rejected; recorded as failed, no retry.
    let m500 = MockNtfy::start(Mode::Status500);
    let e500 = env_for(&m500.url);
    hook(&e500, &stop("s09s", QUESTION));

    // Connection dropped after the request was written: never retried.
    let mdrop = MockNtfy::start(Mode::Drop);
    let edrop = env_for(&mdrop.url);
    hook(&edrop, &stop("s09d", QUESTION));

    assert!(eventually(secs(12.0), || status(&refused, "s09b") == "failed"));
    let ep = episode(&refused, "s09b");
    assert_eq!(ep["attempts"], 3);
    assert_eq!(ep["closed_reason"], "unreachable");
    let doctor = refused.run(&["doctor"]);
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("UNREACHABLE"), "{text}");

    assert!(eventually(secs(5.0), || status(&e500, "s09s") == "failed"));
    assert_eq!(episode(&e500, "s09s")["closed_reason"], "http_status");
    assert!(eventually(secs(5.0), || status(&edrop, "s09d") == "sent_uncertain"));
    let doctor = edrop.run(&["doctor"]);
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("sent_uncertain"));
    settle(&m500, secs(1.0), 1);
}

#[test]
fn s11_project_config_cannot_redirect() {
    let mock = MockNtfy::start(Mode::Ok);
    let evil = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    let proj = TempDir::new("tap-evil");
    std::fs::create_dir_all(proj.path().join(".claude")).unwrap();
    std::fs::write(
        proj.path().join(".claude/tap-on-shoulder.toml"),
        format!(
            "[notify.ntfy]\nserver = \"{}\"\ntopic = \"evil-topic\"\n[training]\ncollect = true\n[classifier]\nmode = \"rules\"\n",
            evil.url
        ),
    )
    .unwrap();
    let cwd = proj.path().to_str().unwrap().to_string();
    hook(&env, &with_cwd(stop("s11", QUESTION), &cwd));
    assert_eq!(mock.wait_for(1, secs(5.0)), 1);
    assert_eq!(mock.requests()[0].json()["topic"], "tap-test-topic");
    settle(&evil, secs(0.5), 0);
    assert!(env.log_text().contains("project_key_ignored"));
    assert!(!env.data.path().join("training").exists());

    // Project grace is clamped to the user's value (default 5m) or lower.
    let mut prod = env_for(&mock.url);
    prod.unset("TAP_DEV");
    prod.unset("TAP_GRACE_OVERRIDE");
    std::fs::write(
        proj.path().join(".claude/tap-on-shoulder.toml"),
        "[grace]\nquestion = \"1h\"\nblocked = \"10s\"\n",
    )
    .unwrap();
    hook(&prod, &with_cwd(stop("s11q", QUESTION), &cwd));
    let ep = episode(&prod, "s11q");
    assert_eq!(
        ep["due_at"].as_i64().unwrap() - ep["opened_at"].as_i64().unwrap(),
        300
    );
    hook(&prod, &with_cwd(stop_failure("s11b"), &cwd));
    let ep = episode(&prod, "s11b");
    assert_eq!(ep["category"], "blocked");
    assert_eq!(
        ep["due_at"].as_i64().unwrap() - ep["opened_at"].as_i64().unwrap(),
        10
    );

    // An oversized project file is ignored entirely.
    let big = format!("[alerts]\ncompletion = true\n#{}\n", "x".repeat(70 * 1024));
    std::fs::write(proj.path().join(".claude/tap-on-shoulder.toml"), big).unwrap();
    let mut big_env = env_for(&mock.url);
    big_env.set("TAP_TEST_FORCE_CATEGORY", "done");
    hook(&big_env, &with_cwd(stop("s11g", "Done."), &cwd));
    assert_eq!(episode(&big_env, "s11g")["closed_reason"], "suppressed");
}

#[test]
fn s12_full_flow_fast() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    let start = Instant::now();
    hook(&env, &stop("s12", QUESTION));
    assert_eq!(mock.wait_for(1, secs(3.0)), 1);
    assert!(start.elapsed() < secs(3.0), "{:?}", start.elapsed());
}

#[test]
fn s12_grace_override_requires_dev() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.unset("TAP_DEV");
    hook(&env, &stop("s12g", QUESTION));
    let ep = episode(&env, "s12g");
    assert_eq!(
        ep["due_at"].as_i64().unwrap() - ep["opened_at"].as_i64().unwrap(),
        300
    );
    assert!(env.log_text().contains("env_ignored"));
    // Clean up so the sweeper exits instead of waiting 5 minutes.
    hook(&env, &session_end("s12g"));
    settle(&mock, secs(2.0), 0);
}

#[test]
fn s_unknown_state_no_tap() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    hook(&env, &session_start("su"));
    let path = env.state_dir().join("su.json");
    std::fs::write(&path, "{\"version\":1, \"episode\": {\"status\": \"open\"").unwrap();

    // Non-arming events and sweepers leave it alone and never tap.
    hook(&env, &post_tool("su", "Bash", "t1", json!({})));
    hook(&env, &permission_prompt("su"));
    assert!(env.run(&["sweep"]).status.success());
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .starts_with("{\"version\":1, \"episode\""));
    assert!(env.log_text().contains("corrupt_state"));
    let doctor = env.run(&["doctor"]);
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("unknown"));
    std::thread::sleep(secs(2.0));
    let posts = mock
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(posts, 0, "unknown state must never tap");

    // The next arming event replaces the file.
    hook(&env, &stop("su", QUESTION));
    assert_eq!(status(&env, "su"), "open");
    assert!(eventually(secs(5.0), || mock
        .requests()
        .iter()
        .any(|r| r.method == "POST")));
}

#[test]
fn sweeper_exit_race_no_orphan() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    // Arm a new episode right around the moment the previous sweeper decides
    // to exit; every episode must still be delivered.
    for i in 0..5 {
        let s = format!("race{i}");
        hook(&env, &stop(&s, QUESTION));
        assert_eq!(
            mock.wait_for(i + 1, secs(5.0)),
            i + 1,
            "episode {i} orphaned"
        );
        std::thread::sleep(Duration::from_millis(30 * i as u64));
    }
    settle(&mock, secs(1.0), 5);
}

fn all_fixtures(s: &str) -> Vec<Value> {
    let input = json!({"command": "ls"});
    vec![
        session_start(s),
        user_prompt(s),
        pre_tool(s, "Bash", "t1", input.clone()),
        permission_request(s, "Bash", input.clone()),
        permission_prompt(s),
        post_tool(s, "Bash", "t1", input.clone()),
        with(
            base("PostToolUseFailure", s, CWD),
            json!({"tool_name": "Bash", "tool_use_id": "t1"}),
        ),
        with(
            base("PermissionDenied", s, CWD),
            json!({"tool_name": "Bash"}),
        ),
        pre_tool(
            s,
            "AskUserQuestion",
            "q1",
            ask_user_question_input("Pick one?"),
        ),
        with(
            base("Notification", s, CWD),
            json!({"notification_type": "elicitation_dialog", "message": "MCP asks"}),
        ),
        base("ElicitationResult", s, CWD),
        with(
            base("Notification", s, CWD),
            json!({"notification_type": "idle_prompt", "message": "idle"}),
        ),
        with(base("SubagentStop", s, CWD), json!({"agent_id": "a1"})),
        stop(s, QUESTION),
        stop_with_bg(s, "Working in the background."),
        stop_failure(s),
        session_end(s),
    ]
}

#[test]
fn hook_stdout_empty_exit0_all_fixtures() {
    let url = MockNtfy::refused_url();
    let mut env = env_for(&url);
    env.set("TAP_GRACE_OVERRIDE", "1h");
    for f in all_fixtures("hs") {
        assert_quiet(&env.hook(&f));
    }
    let big = pre_tool("hs", "Write", "w1", json!({"content": "x".repeat(2 << 20)}));
    assert_quiet(&env.hook(&big));
    let malformed: Vec<(&str, Vec<u8>)> = vec![
        ("Stop", b"".to_vec()),
        ("Stop", b"{".to_vec()),
        ("Stop", b"null".to_vec()),
        ("Stop", b"[]".to_vec()),
        ("Stop", vec![0xff, 0xfe, 0x00, 0x01]),
        ("Stop", br#"{"hook_event_name":"Stop"}"#.to_vec()),
        (
            "Stop",
            br#"{"hook_event_name":"Stop","session_id":"../../x","cwd":"/"}"#.to_vec(),
        ),
        (
            "Stop",
            br#"{"hook_event_name":"Stop","session_id":42}"#.to_vec(),
        ),
        (
            "Bogus",
            br#"{"hook_event_name":"Bogus","session_id":"hs"}"#.to_vec(),
        ),
        ("", b"{}".to_vec()),
    ];
    for (ev, bytes) in &malformed {
        assert_quiet(&env.hook_raw(ev, bytes));
    }
    assert!(!env.data.path().join("x.json").exists());
    assert!(env.log_text().contains("bad_input"));
    assert!(env.log_text().contains("bad_session_id"));
    hook(&env, &session_end("hs"));

    // Without a topic, SessionStart prints exactly one systemMessage object.
    let no_topic = env_for(&url);
    no_topic.write_user_config("[alerts]\ncompletion = false\n");
    let out = no_topic.hook(&session_start("nt"));
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("systemMessage JSON");
    let obj = v.as_object().unwrap();
    assert_eq!(obj.len(), 1);
    assert!(obj["systemMessage"]
        .as_str()
        .unwrap()
        .contains("no ntfy topic"));
    for f in all_fixtures("nt").into_iter().skip(1) {
        assert_quiet(&no_topic.hook(&f));
    }
}

#[test]
fn log_has_no_message_text() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    let secret = "SECRETMARKER";
    let s = "lg";
    let inputs = vec![
        session_start(s),
        user_prompt(s),
        pre_tool(
            s,
            "Bash",
            "t1",
            json!({"command": format!("echo {secret}")}),
        ),
        permission_request(s, "Bash", json!({"command": format!("echo {secret}")})),
        with(
            base("Notification", s, CWD),
            json!({"notification_type": "permission_prompt", "message": secret}),
        ),
        post_tool(s, "Bash", "t1", json!({})),
        pre_tool(
            s,
            "AskUserQuestion",
            "q1",
            ask_user_question_input(&format!("{secret}?")),
        ),
        with(
            base("Notification", s, CWD),
            json!({"notification_type": "elicitation_dialog", "message": secret}),
        ),
        stop(s, &format!("Question {secret}: continue?")),
    ];
    for i in &inputs {
        hook(&env, i);
    }
    assert_quiet(&env.hook_raw("Stop", format!("{{\"session_id\":\"{secret}\",").as_bytes()));
    assert_quiet(&env.hook_raw(
        "Stop",
        format!("{{\"hook_event_name\":\"{secret}\",\"session_id\":\"x\"}}").as_bytes(),
    ));
    assert_eq!(mock.wait_for(1, secs(5.0)), 1);
    assert!(eventually(secs(2.0), || env
        .log_text()
        .contains("notified")));

    let log = env.log_text();
    assert!(!log.is_empty());
    assert!(!log.contains(secret), "message text leaked into the log");
    assert!(!log.contains("PROMPT_TEXT"));
    assert!(!log.contains("payments-service"));
    let allowed = [
        "ts",
        "event",
        "session",
        "episode",
        "transition",
        "category",
        "kind",
    ];
    for line in log.lines() {
        let v: Value = serde_json::from_str(line).expect("log line is JSON");
        for k in v.as_object().unwrap().keys() {
            assert!(allowed.contains(&k.as_str()), "unexpected log key {k}");
        }
    }
}

#[test]
fn parallel_hooks_20_no_corruption() {
    let url = MockNtfy::refused_url();
    let env = env_for(&url);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..20)
            .map(|i| {
                let env = &env;
                scope.spawn(move || {
                    env.hook(&pre_tool(
                        "par",
                        "Bash",
                        &format!("t{i}"),
                        json!({"command": i}),
                    ))
                })
            })
            .collect();
        for h in handles {
            assert_quiet(&h.join().unwrap());
        }
    });
    let st = env.state_json("par").expect("state parses");
    let ids: Vec<String> = st["pending_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["tool_use_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 20, "lost updates: {ids:?}");
    for i in 0..20 {
        assert!(ids.contains(&format!("t{i}")));
    }
    let leftovers = std::fs::read_dir(env.state_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn training_collect_and_reply() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    env.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\n[training]\ncollect = true\n",
        mock.url
    ));
    hook(
        &env,
        &stop(
            "tr",
            "Mail ops@example.com about /srv/app/config.yml. Proceed?",
        ),
    );
    hook(&env, &user_prompt("tr"));
    let path = env.data.path().join("training/collected.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let v: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert!(v["reply_after_s"].is_i64(), "{v}");
    let t = v["text"].as_str().unwrap();
    assert!(
        !t.contains("ops@example.com") && !t.contains("/srv/app"),
        "{t}"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // `tap label` writes labeled.jsonl.
    let mut child = env
        .command(&["label"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"1\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let labeled = std::fs::read_to_string(env.data.path().join("training/labeled.jsonl")).unwrap();
    assert!(labeled.contains("\"label\":\"needs_decision\""));
}

#[test]
fn cli_setup_doctor_version() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);

    let v = env.run(&["--version"]);
    let text = String::from_utf8_lossy(&v.stdout);
    assert!(text.starts_with("tap "));
    assert!(text.contains("model: "));
    assert!(text.contains("feature_spec: 1"));

    // Valid config: doctor exits 0 and reaches the mock with HEAD.
    let d = env.run(&["doctor"]);
    assert_eq!(
        d.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&d.stdout)
    );
    assert!(mock.requests().iter().any(|r| r.method == "HEAD"));

    // `tap test` sends one tap.
    assert!(env.run(&["test"]).status.success());
    assert!(mock.requests().iter().any(|r| r.method == "POST"));

    // Fresh config home: doctor exits 1 with "no ntfy topic".
    let fresh = env_for(&mock.url);
    std::fs::remove_file(fresh.config_dir().join("config.toml")).unwrap();
    let d = fresh.run(&["doctor"]);
    assert_eq!(d.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&d.stdout).contains("no ntfy topic"));

    // Setup writes a 0600 config with a random topic and won't overwrite.
    let s = fresh.run(&["setup", "--server", &mock.url]);
    assert!(s.status.success());
    let out = String::from_utf8_lossy(&s.stdout);
    assert!(out.contains(&format!("{}/tap-", mock.url)), "{out}");
    let cfg = fresh.config_dir().join("config.toml");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let first = std::fs::read_to_string(&cfg).unwrap();
    assert!(fresh.run(&["setup"]).status.success());
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), first);
    assert!(fresh
        .run(&["setup", "--force", "--server", &mock.url])
        .status
        .success());
    assert_ne!(std::fs::read_to_string(&cfg).unwrap(), first);
    assert_eq!(fresh.run(&["doctor"]).status.code(), Some(0));

    // Plain http to a public host is refused.
    assert_eq!(
        fresh
            .run(&["setup", "--force", "--server", "http://ntfy.sh"])
            .status
            .code(),
        Some(2)
    );

    // An invalid user config makes doctor exit 1.
    env.write_user_config("[grace]\nquestion = \"forever\"\n[notify.ntfy]\ntopic = \"t\"\n");
    assert_eq!(env.run(&["doctor"]).status.code(), Some(1));
}
