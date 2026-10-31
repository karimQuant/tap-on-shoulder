//! Away-aware taps (plan v0.2 part B, acceptance criteria 8–13). Presence is
//! steered by the debug-only knobs `TAP_TEST_PRESENCE`,
//! `TAP_TEST_PRESENCE_FILE` (re-read on every probe, so it can change
//! mid-wait) and `TAP_TEST_PRESENCE_SLEEP_MS` (a probe that hangs).

mod support;

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use support::*;

const QUESTION: &str =
    "I can keep the legacy column or drop it.\n\nShould I drop the legacy `old_email` column?";

fn secs(n: f64) -> Duration {
    Duration::from_secs_f64(n)
}

/// Category grace `grace` (via the override), `grace.away = away`, Claude
/// detection off, and `presence` as the knob value (if any).
fn env_with(url: &str, grace: &str, away: &str, presence: Option<&str>) -> TestEnv {
    let mut env = TestEnv::new(url);
    env.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{url}\"\ntopic = \"tap-test-topic\"\n[grace]\naway = \"{away}\"\n"
    ));
    env.set("TAP_TEST_CLAUDE_PID", "none");
    env.set("TAP_GRACE_OVERRIDE", grace);
    if let Some(p) = presence {
        env.set("TAP_TEST_PRESENCE", p);
    }
    env
}

fn hook(env: &TestEnv, input: &Value) {
    let out = env.hook(input);
    assert!(out.status.success(), "hook exit status: {:?}", out.status);
    assert!(out.stdout.is_empty(), "hook stdout must be empty");
}

fn episode(env: &TestEnv, s: &str) -> Value {
    env.state_json(s)
        .map(|v| v["episode"].clone())
        .unwrap_or(Value::Null)
}

fn log_lines(env: &TestEnv) -> Vec<Value> {
    env.log_text()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn count_log(env: &TestEnv, field: &str, value: &str) -> usize {
    log_lines(env).iter().filter(|l| l[field] == value).count()
}

/// Seconds from `start` until the mock saw its first request, or `None`.
fn first_tap_after(mock: &MockNtfy, start: Instant, timeout: Duration) -> Option<f64> {
    eventually(timeout, || mock.count() >= 1).then(|| start.elapsed().as_secs_f64())
}

#[test]
fn s20_away_taps_early() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_with(&mock.url, "10s", "1s", Some("away"));
    let start = Instant::now();
    hook(&env, &stop("s20", QUESTION));
    assert_eq!(episode(&env, "s20")["grace_away_s"], 1);
    let t = first_tap_after(&mock, start, secs(8.0)).expect("away tap");
    assert!(t < 8.0, "away tap came after {t:.1}s");
    std::thread::sleep(secs(2.0));
    assert_eq!(mock.count(), 1, "exactly one tap");
    let ep = episode(&env, "s20");
    assert_eq!(ep["status"], "notified");
    assert_eq!(ep["sent_early"], true, "{ep}");
    assert_eq!(count_log(&env, "kind", "away"), 1, "{}", env.log_text());

    // The idle ledger records the early tap once the user replies.
    hook(&env, &user_prompt("s20"));
    let ledger = std::fs::read_to_string(env.data.path().join("stats/waits.jsonl")).unwrap();
    let lines: Vec<Value> = ledger
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{ledger}");
    assert_eq!(lines[0]["early"], true, "{ledger}");
    assert_eq!(lines[0]["reason"], "user_prompt");
}

#[test]
fn s21_present_keeps_grace() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_with(&mock.url, "8s", "1s", Some("present"));
    let start = Instant::now();
    hook(&env, &stop("s21", QUESTION));
    std::thread::sleep(secs(6.0));
    assert_eq!(mock.count(), 0, "present: no early tap");
    let t = first_tap_after(&mock, start, secs(16.0)).expect("normal tap");
    assert!(t >= 7.0, "tap after {t:.1}s");
    std::thread::sleep(secs(1.0));
    assert_eq!(mock.count(), 1);
    assert_eq!(episode(&env, "s21")["sent_early"], false);
}

/// Unknown presence, `presence.mode = "off"` and an SSH session all keep
/// v0.1 timing.
#[test]
fn s22_unknown_and_mode_off_are_v01() {
    let mocks: Vec<MockNtfy> = (0..4).map(|_| MockNtfy::start(Mode::Ok)).collect();
    let unknown = env_with(&mocks[0].url, "8s", "1s", Some("unknown"));
    // No knob at all: the real probe runs, with no D-Bus address in the
    // cleared test environment, so it is unknown.
    let none = env_with(&mocks[1].url, "8s", "1s", None);
    let off = env_with(&mocks[2].url, "8s", "1s", Some("away"));
    off.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\n[grace]\naway = \"1s\"\n[presence]\nmode = \"off\"\n",
        mocks[2].url
    ));
    let mut ssh = env_with(&mocks[3].url, "8s", "1s", Some("away"));
    ssh.set("SSH_CONNECTION", "192.0.2.1 50000 192.0.2.2 22");
    let envs = [&unknown, &none, &off, &ssh];
    let start = Instant::now();
    for (i, e) in envs.iter().enumerate() {
        hook(e, &stop(&format!("s22{i}"), QUESTION));
    }
    assert_eq!(episode(&ssh, "s223")["grace_away_s"], Value::Null);
    std::thread::sleep(secs(6.0));
    for (i, m) in mocks.iter().enumerate() {
        assert_eq!(m.count(), 0, "env {i}: no early tap");
    }
    for (i, m) in mocks.iter().enumerate() {
        assert_eq!(m.wait_for(1, secs(16.0)), 1, "env {i}: normal tap");
        assert!(start.elapsed() >= secs(7.0));
    }
    std::thread::sleep(secs(1.0));
    for (i, (m, e)) in mocks.iter().zip(envs).enumerate() {
        assert_eq!(m.count(), 1, "env {i}");
        let ep = episode(e, &format!("s22{i}"));
        assert_eq!(ep["sent_early"], false, "env {i}: {ep}");
        assert_eq!(count_log(e, "kind", "away"), 0);
    }
}

#[test]
fn s23_leave_mid_wait_taps_once() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_with(&mock.url, "30s", "1s", None);
    let knob = env.data.path().join("presence-knob");
    std::fs::write(&knob, "present").unwrap();
    env.set("TAP_TEST_PRESENCE_FILE", knob.to_str().unwrap());
    let start = Instant::now();
    hook(&env, &stop("s23", QUESTION));
    std::thread::sleep(secs(3.0));
    assert_eq!(mock.count(), 0, "present: no tap yet");
    // The user leaves. The 5 s presence cache delays the reaction a little.
    std::fs::write(&knob, "away").unwrap();
    let t = first_tap_after(&mock, start, secs(20.0)).expect("tap after leaving");
    assert!((3.0..20.0).contains(&t), "tap after {t:.1}s");
    // Coming back does not matter any more; there is no second tap.
    std::fs::write(&knob, "present").unwrap();
    std::thread::sleep(secs(3.0));
    assert_eq!(mock.count(), 1, "taps once");
    let ep = episode(&env, "s23");
    assert_eq!(ep["status"], "notified");
    assert_eq!(ep["sent_early"], true);
}

/// The claim re-checks the due time under the lock; an early (away) tap
/// must pass that check instead of looping on it.
#[test]
fn s24_claim_guard_accepts_early_tap() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_with(&mock.url, "10s", "1s", Some("away"));
    env.set("TAP_TEST_SWEEP_CLAIM_PAUSE_MS", "700");
    let start = Instant::now();
    hook(&env, &stop("s24", QUESTION));
    let t = first_tap_after(&mock, start, secs(9.0)).expect("early tap");
    assert!(t < 9.0);
    std::thread::sleep(secs(1.5));
    assert_eq!(mock.count(), 1);
    assert_eq!(
        count_log(&env, "kind", "test_claim_pause"),
        1,
        "one claim, no retry loop: {}",
        env.log_text()
    );
    assert_eq!(count_log(&env, "transition", "sending"), 1);

    // A re-arm during the claim window that restarts the wait (a new
    // binding) is still rejected by the guard.
    let mock2 = MockNtfy::start(Mode::Ok);
    let mut env2 = env_with(&mock2.url, "10s", "3s", Some("away"));
    env2.set("TAP_TEST_SWEEP_CLAIM_PAUSE_MS", "1500");
    let a = json!({"command": "git push"});
    let b = json!({"command": "rm -rf build"});
    hook(&env2, &pre_tool("s24b", "Bash", "ta", a.clone()));
    hook(&env2, &pre_tool("s24b", "Bash", "tb", b.clone()));
    hook(&env2, &permission_request("s24b", "Bash", a));
    assert!(
        eventually(secs(8.0), || count_log(&env2, "kind", "test_claim_pause")
            >= 1),
        "{}",
        env2.log_text()
    );
    let rebound = Instant::now();
    hook(&env2, &permission_request("s24b", "Bash", b));
    assert_eq!(episode(&env2, "s24b")["tool_use_id"], "tb");
    let t = first_tap_after(&mock2, rebound, secs(9.0)).expect("tap for the new prompt");
    assert!(t >= 1.5, "the stale claim must not send ({t:.1}s)");
    std::thread::sleep(secs(1.0));
    assert_eq!(mock2.count(), 1);
}

/// A process standing in for Claude whose child is the approved tool.
struct FakeClaude {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl FakeClaude {
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
        let pid = self.child.id();
        assert!(eventually(secs(3.0), || {
            std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                .map(|c| !c.trim().is_empty())
                .unwrap_or(true)
        }));
    }
}

impl Drop for FakeClaude {
    fn drop(&mut self) {
        // SAFETY: plain syscall on our own process group.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

/// While away, a postponed permission tap follows its postponed due time:
/// presence cannot pull it forward again, so postponement does not spin.
#[test]
fn s25_postpone_while_away_no_loop() {
    let mut claude = FakeClaude::with_tool();
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_with(&mock.url, "30s", "4s", Some("away"));
    env.set("TAP_TEST_CLAUDE_PID", &claude.pid());
    env.set("TAP_DEV_FAST", "1");
    let input = json!({"command": "long job"});
    let start = Instant::now();
    hook(&env, &pre_tool("s25", "Bash", "t1", input.clone()));
    hook(&env, &permission_request("s25", "Bash", input));
    std::thread::sleep(secs(1.1));
    claude.start_tool();

    // Away after 4 s, then three 1 s postponements (the dev-fast cap): the
    // tap comes at about 8 s. The wait is generous because a loaded machine
    // can stall the sweeper's fsyncs for seconds; that presence pulled the
    // first check forward is asserted on the log timestamps below.
    first_tap_after(&mock, start, secs(60.0))
        .unwrap_or_else(|| panic!("tap after the cap: {}", env.log_text()));
    std::thread::sleep(secs(1.0));
    assert_eq!(mock.count(), 1);
    let ep = episode(&env, "s25");
    assert_eq!(ep["postponed_s"], 3, "{ep}");
    assert_eq!(ep["sent_early"], false, "postponed taps ignore presence");
    let ts: Vec<i64> = log_lines(&env)
        .iter()
        .filter(|l| l["transition"] == "postponed")
        .filter_map(|l| l["ts"].as_i64())
        .collect();
    assert_eq!(ts.len(), 3, "{}", env.log_text());
    let opened = ep["opened_at"].as_i64().unwrap();
    assert!(
        ts[0] < opened + 30,
        "away must pull the first check before the 30 s grace: {ts:?}, opened {opened}"
    );
    assert!(
        ts[2] - ts[0] >= 2,
        "postponements must be spaced by their step: {ts:?}"
    );
}

#[test]
fn s26_probe_timeout_is_unknown() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_with(&mock.url, "8s", "1s", Some("away"));
    // The probe hangs for 5 s; it is killed after 500 ms and reads unknown,
    // even though the plain knob says away.
    env.set("TAP_TEST_PRESENCE_SLEEP_MS", "5000");
    let start = Instant::now();
    hook(&env, &stop("s26", QUESTION));
    assert!(
        eventually(secs(5.0), || count_log(&env, "kind", "presence_timeout")
            >= 1),
        "{}",
        env.log_text()
    );
    std::thread::sleep(secs(6.0).saturating_sub(start.elapsed()));
    assert_eq!(mock.count(), 0, "unknown: no early tap");
    let t = first_tap_after(&mock, start, secs(16.0)).expect("normal tap");
    assert!(t >= 7.0, "tap after {t:.1}s");
    std::thread::sleep(secs(1.0));
    assert_eq!(mock.count(), 1);
    assert_eq!(episode(&env, "s26")["sent_early"], false);
}

#[test]
fn s27_grace_away_above_category_rejected() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = TestEnv::new(&mock.url);
    env.unset("TAP_GRACE_OVERRIDE");
    env.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\n[grace]\nquestion = \"1m\"\naway = \"2m\"\n",
        mock.url
    ));
    let out = env.run(&["doctor"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("error: user config: grace.away: must not exceed any category grace"),
        "{text}"
    );
    assert!(text.contains("config: INVALID"), "{text}");
    assert!(text.contains("grace.away 30s"), "default used: {text}");

    // Within every category grace: valid. The override never raises it.
    env.write_user_config(&format!(
        "[notify.ntfy]\nserver = \"{}\"\ntopic = \"tap-test-topic\"\n[grace]\naway = \"45s\"\n",
        mock.url
    ));
    let out = env.run(&["doctor"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("grace.away 45s"), "{text}");
    env.set("TAP_GRACE_OVERRIDE", "10s");
    let text = String::from_utf8_lossy(&env.run(&["doctor"]).stdout).into_owned();
    assert!(text.contains("grace.away 10s"), "clamped: {text}");
}

#[test]
fn s28_project_config_cannot_set_presence() {
    let mock = MockNtfy::start(Mode::Ok);
    let env = env_with(&mock.url, "1h", "20s", Some("present"));
    let proj = TempDir::new("tap-proj");
    std::fs::create_dir_all(proj.path().join(".claude")).unwrap();
    std::fs::write(
        proj.path().join(".claude").join("tap-on-shoulder.toml"),
        "[presence]\nmode = \"off\"\nidle_away = \"10s\"\n[grace]\naway = \"5s\"\n",
    )
    .unwrap();
    let out = env
        .command(&["doctor"])
        .current_dir(proj.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for key in ["presence.mode", "presence.idle_away"] {
        assert!(
            text.contains(&format!(
                "warning: project config: {key}: not allowed in a project file, ignored"
            )),
            "{text}"
        );
    }
    assert!(
        text.contains("presence: source test"),
        "mode stays auto: {text}"
    );
    assert!(text.contains("grace.away 5s"), "shortened: {text}");

    let cwd = proj.path().to_str().unwrap();
    let mut input = stop("s28", QUESTION);
    input["cwd"] = json!(cwd);
    hook(&env, &input);
    assert_eq!(episode(&env, "s28")["grace_away_s"], 5);

    // A project value above the user's is clamped to the user's.
    std::fs::write(
        proj.path().join(".claude").join("tap-on-shoulder.toml"),
        "[grace]\naway = \"10m\"\n",
    )
    .unwrap();
    let mut input = stop("s28b", QUESTION);
    input["cwd"] = json!(cwd);
    hook(&env, &input);
    assert_eq!(episode(&env, "s28b")["grace_away_s"], 20);
    hook(&env, &session_end("s28"));
    hook(&env, &session_end("s28b"));
    let posts = mock
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(posts, 0, "no tap");
}

/// Spike S11 (manual, GNOME): a real hook spawns the setsid'd sweeper, which
/// probes the session bus itself. Run with the desktop locked or idle:
/// `cargo test --test scenarios_presence s11 -- --ignored --nocapture`
#[test]
#[ignore]
fn s11_live_sweeper_reaches_session_bus() {
    let mock = MockNtfy::start(Mode::Ok);
    let mut env = env_with(&mock.url, "60s", "1s", None);
    for k in ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"] {
        if let Ok(v) = std::env::var(k) {
            env.set(k, &v);
        }
    }
    let start = Instant::now();
    hook(&env, &stop("s11", QUESTION));
    let t = first_tap_after(&mock, start, secs(15.0));
    eprintln!("tap after {t:?} s; log:\n{}", env.log_text());
    assert_eq!(count_log(&env, "kind", "presence_timeout"), 0);
    assert!(t.is_some(), "seat not away, or the bus was not reached");
}
