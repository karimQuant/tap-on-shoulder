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
    // The sweeper pauses between seeing a due episode and claiming it, so
    // the new turn lands exactly in that window.
    env.set("TAP_TEST_SWEEP_CLAIM_PAUSE_MS", "1500");

    hook(&env, &stop("s03", "Old turn: which option should I take?"));
    let first = episode(&env, "s03")["id"].clone();
    // The old episode is due and the sweeper is about to claim it...
    assert!(
        eventually(secs(5.0), || env.log_text().contains("test_claim_pause")),
        "sweeper never reached the claim"
    );
    // ...when Claude resumes and a new turn replaces it.
    let bash = json!({"command": "cargo build"});
    hook(&env, &pre_tool("s03", "Bash", "t1", bash.clone()));
    assert_eq!(episode(&env, "s03")["closed_reason"], "activity");
    hook(&env, &post_tool("s03", "Bash", "t1", bash));
    hook(&env, &stop("s03", "New turn: deploy to staging or prod?"));
    let second = episode(&env, "s03")["id"].clone();
    assert_ne!(first, second, "a newer turn gets a new episode id");
    assert_eq!(mock.count(), 0, "the replaced episode must not be sent");

    // A resumed session (SessionStart) closes a stale episode too.
    hook(&env, &stop("s03r", "Stale: shall I continue?"));
    hook(
        &env,
        &with(session_start("s03r"), json!({"source": "resume"})),
    );
    assert_eq!(episode(&env, "s03r")["closed_reason"], "session_start");

    assert_eq!(mock.wait_for(1, secs(8.0)), 1);
    settle(&mock, secs(2.0), 1);
    let msg = mock.requests()[0].json()["message"].to_string();
    assert!(msg.contains("New turn"), "{msg}");
    let ep = episode(&env, "s03");
    assert_eq!(ep["id"], second);
    assert_eq!(ep["status"], "notified");
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

    // Background work still running and Claude is just working: Stop opens
    // nothing.
    let mut working = env_for(&mock.url);
    working.set("TAP_TEST_FORCE_CATEGORY", "info");
    hook(
        &working,
        &stop_with_bg("s04b", "Started the build in the background."),
    );
    assert!(episode(&working, "s04b").is_null());
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
    // 4 s grace: the tool must start before the first check, even when the
    // machine is loaded by parallel test binaries.
    env.set("TAP_GRACE_OVERRIDE", "4s");

    let input = json!({"command": "make test"});
    hook(&env, &pre_tool("s06b", "Bash", "t1", input.clone()));
    hook(&env, &permission_request("s06b", "Bash", input.clone()));
    // The user approves (which takes over a second); the tool now runs as a
    // child of Claude.
    std::thread::sleep(secs(1.1));
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

    // No hash match and one pending entry of that name: binds to it.
    hook(
        &env,
        &pre_tool("s06f", "Bash", "f1", json!({"command": "a"})),
    );
    hook(
        &env,
        &permission_request("s06f", "Bash", json!({"command": "other"})),
    );
    assert_eq!(episode(&env, "s06f")["tool_use_id"], "f1");
    hook(
        &env,
        &post_tool("s06f", "Bash", "f1", json!({"command": "a"})),
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
    // 4 s grace: the tool must start before the first check, even when the
    // machine is loaded by parallel test binaries.
    env.set("TAP_GRACE_OVERRIDE", "4s");

    let input = json!({"command": "long job"});
    hook(&env, &pre_tool("s06d", "Bash", "t1", input.clone()));
    hook(&env, &permission_request("s06d", "Bash", input));
    std::thread::sleep(secs(1.1));
    claude.start_tool();
    // The tool keeps running past the postponement cap: the tap goes out.
    assert_eq!(mock.wait_for(1, secs(20.0)), 1);
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
    let mut env = env_for(&mock.url);
    // The sweeper pauses after releasing its lock and before the final
    // re-scan: the exact window of the exit race.
    env.set("TAP_TEST_SWEEP_EXIT_PAUSE_MS", "1500");
    hook(&env, &stop("race0", QUESTION));
    assert_eq!(mock.wait_for(1, secs(5.0)), 1);
    assert!(
        eventually(secs(5.0), || env.log_text().contains("test_exit_pause")),
        "sweeper never reached its exit window"
    );

    // An episode opens inside the window. It is written directly, so no hook
    // spawns a fresh sweeper: only the exiting one's re-scan can deliver it.
    let mut st = env.state_json("race0").unwrap();
    st["session_id"] = json!("race1");
    let ep = &mut st["episode"];
    ep["id"] = json!("00000000000race1");
    ep["status"] = json!("open");
    ep["due_at"] = json!(now_s());
    ep["attempts"] = json!(0);
    ep["sending_since"] = Value::Null;
    ep["closed_reason"] = Value::Null;
    write_state(&env, "race1", &st);

    assert_eq!(mock.wait_for(2, secs(5.0)), 2, "episode orphaned");
    settle(&mock, secs(1.0), 2);
    assert_eq!(status(&env, "race1"), "notified");
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

// ---------------------------------------------------------------- review regressions

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Hold a session's state lock like a hook or the sweeper would.
fn lock_session(env: &TestEnv, s: &str) -> std::fs::File {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(env.state_dir().join(format!("{s}.lock")))
        .unwrap();
    // SAFETY: plain flock on a descriptor we own.
    assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
    f
}

/// Replace a session's state atomically (temp file + rename) under its lock.
fn write_state(env: &TestEnv, s: &str, v: &Value) {
    let _l = lock_session(env, s);
    let tmp = env.state_dir().join(format!(".tmp-test-{s}"));
    std::fs::write(&tmp, v.to_string()).unwrap();
    std::fs::rename(&tmp, env.state_dir().join(format!("{s}.json"))).unwrap();
}

fn set_age(path: &std::path::Path, age: Duration) {
    let t = std::time::SystemTime::now() - age;
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn denied(s: &str, tool: &str) -> Value {
    with(base("PermissionDenied", s, CWD), json!({"tool_name": tool}))
}

#[test]
fn rv_sweeper_lock_session_named_sweeper() {
    // A session id equal to the old sweeper lock name must not deadlock.
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    let start = Instant::now();
    hook(&env, &stop("sweeper", QUESTION));
    assert!(start.elapsed() < secs(2.0), "{:?}", start.elapsed());
    assert_eq!(mock.wait_for(1, secs(5.0)), 1, "{}", env.log_text());
    assert!(eventually(secs(2.0), || status(&env, "sweeper") == "notified"));
    assert!(env.data.path().join("sweeper.lock").exists());
    let start = Instant::now();
    hook(&env, &user_prompt("sweeper"));
    assert!(start.elapsed() < secs(2.0), "{:?}", start.elapsed());
}

#[test]
fn rv_hook_lock_timeout() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_for(&mock.url);
    hook(&env, &session_start("rvl"));
    let held = lock_session(&env, "rvl");
    let start = Instant::now();
    let out = env.hook(&stop("rvl", QUESTION));
    let took = start.elapsed();
    assert_quiet(&out);
    assert!(took >= secs(1.9) && took < secs(4.0), "{took:?}");
    assert!(env.log_text().contains("lock_timeout"));
    assert!(episode(&env, "rvl").is_null());
    drop(held);
    settle(&mock, secs(1.5), 0);
}

#[test]
fn rv_stdin_oversize() {
    let env = env_for(&MockNtfy::refused_url());
    let big = pre_tool(
        "rvo",
        "Write",
        "w1",
        json!({"content": "x".repeat(9 << 20)}),
    );
    assert_quiet(&env.hook(&big));
    assert!(
        env.log_text().contains("\"oversize\""),
        "{}",
        env.log_text()
    );
    assert!(env.state_json("rvo").is_none());

    // Just under the cap is still read.
    let ok = pre_tool(
        "rvo",
        "Write",
        "w2",
        json!({"content": "x".repeat(7 << 20)}),
    );
    assert_quiet(&env.hook(&ok));
    let st = env.state_json("rvo").unwrap();
    assert_eq!(st["pending_tools"][0]["tool_use_id"], "w2");
}

#[test]
fn rv_setup_random_failure() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    let cfg = env.config_dir().join("config.toml");
    std::fs::remove_file(&cfg).unwrap();
    env.set("TAP_TEST_GETRANDOM_FAIL", "1");
    let out = env.run(&["setup", "--server", &mock.url]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("random"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("tap-"));
    assert!(!cfg.exists(), "no config with a short topic");

    env.unset("TAP_TEST_GETRANDOM_FAIL");
    assert!(env.run(&["setup", "--server", &mock.url]).status.success());
    let text = std::fs::read_to_string(&cfg).unwrap();
    let topic = regex::Regex::new(r#"topic = "(tap-[a-z0-9]+)""#)
        .unwrap()
        .captures(&text)
        .unwrap()[1]
        .to_string();
    assert_eq!(topic.len(), 4 + 24, "{topic}");
}

#[test]
fn rv_rebind_starts_fresh() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "1h");
    let a = json!({"command": "git push"});
    let b = json!({"command": "rm -rf build"});
    hook(&env, &pre_tool("rvr", "Bash", "t1", a.clone()));
    hook(&env, &pre_tool("rvr", "Bash", "t2", b.clone()));
    hook(&env, &permission_request("rvr", "Bash", a.clone()));
    hook(&env, &permission_prompt("rvr"));
    let first = episode(&env, "rvr");
    assert_eq!(first["tool_use_id"], "t1");
    assert!(first["confirmed_at"].is_i64());
    std::thread::sleep(secs(1.2));

    // Same binding: the episode keeps its clock.
    hook(&env, &permission_request("rvr", "Bash", a));
    let same = episode(&env, "rvr");
    assert_eq!(same["opened_at"], first["opened_at"]);
    assert_eq!(same["confirmed_at"], first["confirmed_at"]);

    // A prompt for a different call: the previous one was answered.
    hook(&env, &permission_request("rvr", "Bash", b));
    let ep = episode(&env, "rvr");
    assert_eq!(ep["tool_use_id"], "t2");
    let opened = ep["opened_at"].as_i64().unwrap();
    assert!(opened > first["opened_at"].as_i64().unwrap(), "{ep}");
    assert_eq!(ep["bound_at"].as_i64(), Some(opened));
    assert_eq!(ep["due_at"].as_i64(), Some(opened + 3600));
    assert_eq!(ep["postponed_s"], 0);
    assert!(ep["confirmed_at"].is_null(), "{ep}");

    // AskUserQuestion rebinding starts fresh too.
    std::thread::sleep(secs(1.2));
    let ask = ask_user_question_input("Which region?");
    hook(&env, &pre_tool("rvr", "AskUserQuestion", "q1", ask));
    let q = episode(&env, "rvr");
    assert_eq!(q["tool_use_id"], "q1");
    assert!(q["opened_at"].as_i64().unwrap() > opened, "{q}");
    hook(&env, &session_end("rvr"));
    settle(&mock, secs(0.5), 0);
}

#[test]
fn rv_stop_background_needs_user() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut ask = env_for(&mock.url);
    ask.set("TAP_TEST_FORCE_CATEGORY", "needs_decision");
    hook(
        &ask,
        &stop_with_bg("rvb", "Build is running. Deploy to prod?"),
    );
    assert_eq!(status(&ask, "rvb"), "open");

    let mut blocked = env_for(&mock.url);
    blocked.set("TAP_TEST_FORCE_CATEGORY", "blocked");
    hook(
        &blocked,
        &stop_with_bg("rvbb", "Build is running. Missing API key."),
    );
    assert_eq!(episode(&blocked, "rvbb")["category"], "blocked");

    // `done` with background work means "working", even with completion taps.
    let mut done = env_for(&mock.url);
    done.set("TAP_TEST_FORCE_CATEGORY", "done");
    done.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\n[alerts]\ncompletion = true\n",
        mock.url
    ));
    hook(&done, &stop_with_bg("rvbd", "Step one finished."));
    assert!(episode(&done, "rvbd").is_null());
    assert!(done.log_text().contains("background_running"));

    assert_eq!(mock.wait_for(2, secs(6.0)), 2);
    settle(&mock, secs(1.0), 2);
}

#[test]
fn rv_ambiguous_binding() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "1h");

    // Two pending Bash calls, neither matching: bind to both.
    hook(
        &env,
        &pre_tool("rva", "Bash", "f1", json!({"command": "a"})),
    );
    hook(
        &env,
        &pre_tool("rva", "Bash", "f2", json!({"command": "b"})),
    );
    hook(
        &env,
        &permission_request("rva", "Bash", json!({"command": "other"})),
    );
    let ep = episode(&env, "rva");
    assert!(ep["tool_use_id"].is_null(), "{ep}");
    assert_eq!(ep["candidate_ids"], json!(["f1", "f2"]));
    hook(
        &env,
        &post_tool("rva", "Bash", "f2", json!({"command": "b"})),
    );
    assert_eq!(status(&env, "rva"), "open", "f1 may still be waiting");
    hook(
        &env,
        &pre_tool("rva", "Read", "r1", json!({"file_path": "x"})),
    );
    hook(
        &env,
        &post_tool("rva", "Read", "r1", json!({"file_path": "x"})),
    );
    assert_eq!(status(&env, "rva"), "open");
    hook(
        &env,
        &post_tool("rva", "Bash", "f1", json!({"command": "a"})),
    );
    assert_eq!(episode(&env, "rva")["closed_reason"], "tool_done");

    // No pending call of that tool: bound by name.
    hook(
        &env,
        &permission_request("rvn", "Bash", json!({"command": "x"})),
    );
    let ep = episode(&env, "rvn");
    assert_eq!(ep["bound_tool_name"], "Bash");
    assert!(ep["tool_use_id"].is_null());
    // Other activity does not close it (no "any activity" rule)...
    hook(
        &env,
        &pre_tool("rvn", "Read", "r1", json!({"file_path": "x"})),
    );
    hook(
        &env,
        &post_tool("rvn", "Read", "r1", json!({"file_path": "x"})),
    );
    assert_eq!(status(&env, "rvn"), "open");
    // ...but the same tool finishing does.
    hook(
        &env,
        &post_tool("rvn", "Bash", "z9", json!({"command": "x"})),
    );
    assert_eq!(episode(&env, "rvn")["closed_reason"], "tool_done");
    settle(&mock, secs(0.5), 0);
}

#[test]
fn rv_claim_rechecks_due() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_TEST_SWEEP_CLAIM_PAUSE_MS", "2000");
    hook(&env, &stop("rvc", QUESTION));
    assert!(eventually(secs(5.0), || env
        .log_text()
        .contains("test_claim_pause")));
    // Between the sweeper's due check and its claim, the episode is pushed
    // into the future (same id, status, and attempts).
    let mut st = env.state_json("rvc").unwrap();
    st["episode"]["due_at"] = json!(now_s() + 3600);
    write_state(&env, "rvc", &st);
    settle(&mock, secs(3.5), 0);
    assert_eq!(status(&env, "rvc"), "open");
    hook(&env, &session_end("rvc"));
}

#[test]
fn rv_prune_keeps_live_locks() {
    let env = env_for(&MockNtfy::refused_url());
    let day2 = Duration::from_secs(48 * 3600);
    let dir = env.state_dir();
    // A live session whose lock file is old but whose state is fresh.
    hook(&env, &pre_tool("live", "Bash", "t1", json!({})));
    set_age(&dir.join("live.lock"), day2);
    // A stale session: state and lock both old.
    hook(&env, &pre_tool("stale", "Bash", "t1", json!({})));
    set_age(&dir.join("stale.json"), day2);
    set_age(&dir.join("stale.lock"), day2);
    // Orphaned locks, old and new.
    std::fs::write(dir.join("orphanold.lock"), "").unwrap();
    set_age(&dir.join("orphanold.lock"), day2);
    std::fs::write(dir.join("orphannew.lock"), "").unwrap();

    hook(&env, &session_start("pruner"));
    assert!(dir.join("live.lock").exists(), "live lock deleted");
    assert!(dir.join("live.json").exists());
    assert!(!dir.join("stale.json").exists());
    assert!(!dir.join("stale.lock").exists());
    assert!(!dir.join("orphanold.lock").exists());
    assert!(dir.join("orphannew.lock").exists());
}

#[test]
fn rv_zombie_session_gone() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    // Exits after 1 s and stays a zombie: it is not reaped until the end.
    let mut child = Command::new("sleep").arg("1").spawn().unwrap();
    env.set("TAP_TEST_CLAUDE_PID", &child.id().to_string());
    env.set("TAP_GRACE_OVERRIDE", "3s");
    hook(&env, &stop("rvz", QUESTION));
    assert_eq!(
        env.state_json("rvz").unwrap()["claude"]["pid"].to_string(),
        child.id().to_string()
    );
    assert!(eventually(secs(7.0), || episode(&env, "rvz")
        ["closed_reason"]
        == "session_gone"));
    settle(&mock, secs(0.5), 0);
    let _ = child.wait();
}

#[test]
fn rv_detect_claude_once() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut a = FakeClaude::idle();
    let b = FakeClaude::idle();
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "1h");
    let claude = |env: &TestEnv| env.state_json("rvd").unwrap()["claude"]["pid"].to_string();

    env.set("TAP_TEST_CLAUDE_PID", &a.pid());
    hook(&env, &stop("rvd", QUESTION));
    assert_eq!(claude(&env), a.pid());
    hook(&env, &user_prompt("rvd"));

    // Detection is not repeated while the recorded process lives.
    env.set("TAP_TEST_CLAUDE_PID", &b.pid());
    hook(&env, &stop("rvd", QUESTION));
    assert_eq!(claude(&env), a.pid());
    hook(&env, &user_prompt("rvd"));

    // Once it is gone, the next episode detects again.
    a.kill();
    hook(&env, &stop("rvd", QUESTION));
    assert_eq!(claude(&env), b.pid());
    hook(&env, &session_end("rvd"));
    settle(&mock, secs(0.5), 0);
}

#[test]
fn rv_permission_denied_own_tool() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_GRACE_OVERRIDE", "1h");

    // Bound by id.
    let ls = json!({"command": "ls"});
    hook(&env, &pre_tool("rvp", "Bash", "t1", ls.clone()));
    hook(&env, &permission_request("rvp", "Bash", ls));
    hook(&env, &denied("rvp", "Write"));
    assert_eq!(status(&env, "rvp"), "open");
    hook(&env, &denied("rvp", "Bash"));
    assert_eq!(episode(&env, "rvp")["closed_reason"], "permission_denied");

    // Bound to candidates.
    hook(
        &env,
        &pre_tool("rvpc", "Bash", "c1", json!({"command": "a"})),
    );
    hook(
        &env,
        &pre_tool("rvpc", "Bash", "c2", json!({"command": "b"})),
    );
    hook(
        &env,
        &permission_request("rvpc", "Bash", json!({"command": "z"})),
    );
    hook(&env, &denied("rvpc", "Read"));
    assert_eq!(status(&env, "rvpc"), "open");
    hook(&env, &denied("rvpc", "Bash"));
    assert_eq!(status(&env, "rvpc"), "closed");

    // Bound by name.
    hook(
        &env,
        &permission_request("rvpn", "Bash", json!({"command": "z"})),
    );
    hook(&env, &denied("rvpn", "Edit"));
    assert_eq!(status(&env, "rvpn"), "open");
    hook(&env, &denied("rvpn", "Bash"));
    assert_eq!(status(&env, "rvpn"), "closed");

    // Unbound: any denial closes, as before.
    hook(&env, &stop("rvpu", QUESTION));
    hook(&env, &denied("rvpu", "Bash"));
    assert_eq!(status(&env, "rvpu"), "closed");
    settle(&mock, secs(0.5), 0);
}

#[test]
fn rv_postpone_window() {
    let mut claude = FakeClaude::with_tool();
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_for(&mock.url);
    env.set("TAP_TEST_CLAUDE_PID", &claude.pid());
    env.set("TAP_DEV_FAST", "1");
    env.set("TAP_GRACE_OVERRIDE", "2s");

    // A parallel tool started just before the prompt, in the same wall-clock
    // second, is not the approved tool: no postponement.
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_millis() as u64;
    std::thread::sleep(Duration::from_millis(1000 - ms + 20));
    claude.start_tool();
    let input = json!({"command": "deploy"});
    hook(&env, &pre_tool("rvw", "Bash", "t1", input.clone()));
    hook(&env, &permission_request("rvw", "Bash", input));
    assert_eq!(mock.wait_for(1, secs(6.0)), 1, "{}", env.log_text());
    assert_eq!(episode(&env, "rvw")["postponed_s"], 0);
}

#[test]
fn rv_write_guard_close_during_send() {
    let mock = MockNtfy::start(Mode::Ok);
    mock.set_delay(secs(2.0));
    let env = env_for(&mock.url);
    hook(&env, &stop("rvg", QUESTION));
    assert_eq!(mock.wait_for(1, secs(5.0)), 1);
    assert_eq!(status(&env, "rvg"), "sending");
    // The user replies while the POST is in flight: the hook wins.
    hook(&env, &user_prompt("rvg"));
    assert_eq!(status(&env, "rvg"), "closed");
    assert!(eventually(secs(4.0), || env
        .log_text()
        .contains("guard_lost")));
    let ep = episode(&env, "rvg");
    assert_eq!(ep["status"], "closed");
    assert_eq!(ep["closed_reason"], "user_prompt");
    settle(&mock, secs(1.0), 1);
}
