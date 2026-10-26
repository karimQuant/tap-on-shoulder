//! Shared test support (owned by Lead). Integration tests do `mod support;`.
//!
//! - [`TestEnv`]: isolated `CLAUDE_PLUGIN_DATA` + `XDG_CONFIG_HOME` with a user
//!   config pointing at a [`MockNtfy`], and helpers to run `tap` like a hook.
//! - Fixture builders produce hook stdin JSON shaped like Claude Code 2.1.310
//!   (see plan S1–S10).

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const BIN: &str = env!("CARGO_BIN_EXE_tap");

// ---------------------------------------------------------------- temp dirs

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).expect("random");
        let name = format!(
            "{prefix}-{}",
            buf.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path).expect("create temp dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- mock ntfy

/// Behaviour of the mock server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Ok = 0,
    Status500 = 1,
    /// Accept then immediately drop the connection without a response.
    Drop = 2,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// Minimal HTTP/1.1 server recording every request.
pub struct MockNtfy {
    pub url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    mode: Arc<AtomicU8>,
    /// Milliseconds to hold each response after recording the request.
    delay_ms: Arc<AtomicU64>,
}

impl MockNtfy {
    pub fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mode = Arc::new(AtomicU8::new(mode as u8));
        let delay_ms = Arc::new(AtomicU64::new(0));
        let (reqs, m, d) = (requests.clone(), mode.clone(), delay_ms.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (reqs, m, d) = (reqs.clone(), m.clone(), d.clone());
                std::thread::spawn(move || handle(stream, &reqs, &m, &d));
            }
        });
        MockNtfy {
            url,
            requests,
            mode,
            delay_ms,
        }
    }

    /// Hold every response for `d` after the request is recorded, so a send
    /// stays in flight.
    pub fn set_delay(&self, d: Duration) {
        self.delay_ms.store(d.as_millis() as u64, Ordering::SeqCst);
    }

    /// A URL where nothing listens (connection refused).
    pub fn refused_url() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        url
    }

    pub fn set_mode(&self, mode: Mode) {
        self.mode.store(mode as u8, Ordering::SeqCst);
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// Wait until at least `n` requests arrived or `timeout` passes.
    pub fn wait_for(&self, n: usize, timeout: Duration) -> usize {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.count() >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.count()
    }
}

fn handle(stream: TcpStream, reqs: &Mutex<Vec<Request>>, mode: &AtomicU8, delay_ms: &AtomicU64) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.trim_end().split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            if k == "content-length" {
                len = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }
    let mut body = vec![0u8; len.min(1 << 20)];
    let _ = reader.read_exact(&mut body);
    let current = mode.load(Ordering::SeqCst);
    if current == Mode::Drop as u8 {
        return;
    }
    reqs.lock().unwrap().push(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let delay = delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        std::thread::sleep(Duration::from_millis(delay));
    }
    let mut out = stream;
    let resp: &[u8] = if current == Mode::Status500 as u8 {
        b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    } else {
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}"
    };
    let _ = out.write_all(resp);
}

// ---------------------------------------------------------------- test env

pub struct TestEnv {
    pub data: TempDir,
    pub config_home: TempDir,
    pub ntfy_url: String,
    /// Extra env applied to every `tap` invocation.
    pub env: Vec<(String, String)>,
}

impl TestEnv {
    /// Fresh env whose user config targets `ntfy_url` with a 1 s grace override.
    pub fn new(ntfy_url: &str) -> Self {
        let env = TestEnv {
            data: TempDir::new("tap-data"),
            config_home: TempDir::new("tap-cfg"),
            ntfy_url: ntfy_url.to_string(),
            env: vec![
                ("TAP_DEV".into(), "1".into()),
                ("TAP_GRACE_OVERRIDE".into(), "1s".into()),
            ],
        };
        env.write_user_config(&format!(
            "[notify.ntfy]\nserver = \"{ntfy_url}\"\ntopic = \"tap-test-topic\"\n"
        ));
        env
    }

    pub fn config_dir(&self) -> PathBuf {
        self.config_home.path().join("tap-on-shoulder")
    }

    pub fn write_user_config(&self, toml: &str) {
        let dir = self.config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), toml).unwrap();
    }

    pub fn state_dir(&self) -> PathBuf {
        self.data.path().join("state")
    }

    pub fn state_json(&self, session: &str) -> Option<Value> {
        let p = self.state_dir().join(format!("{session}.json"));
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(self.data.path().join("log").join("tap.log")).unwrap_or_default()
    }

    pub fn set(&mut self, k: &str, v: &str) {
        self.env.retain(|(key, _)| key != k);
        self.env.push((k.into(), v.into()));
    }

    pub fn unset(&mut self, k: &str) {
        self.env.retain(|(key, _)| key != k);
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.config_home.path())
            .env("CLAUDE_PLUGIN_DATA", self.data.path())
            .env("XDG_CONFIG_HOME", self.config_home.path());
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    /// Run `tap hook <event>` with `input` on stdin, like Claude Code does.
    pub fn hook(&self, input: &Value) -> Output {
        let event = input["hook_event_name"].as_str().expect("hook_event_name");
        self.hook_raw(event, input.to_string().as_bytes())
    }

    pub fn hook_raw(&self, event: &str, stdin: &[u8]) -> Output {
        self.run_stdin(&["hook", event], stdin)
    }

    /// Run `tap <args>` with `stdin` piped in.
    pub fn run_stdin(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tap");
        child.stdin.take().unwrap().write_all(stdin).ok();
        child.wait_with_output().expect("wait tap")
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run tap")
    }
}

// ---------------------------------------------------------------- fixtures

pub fn base(event: &str, session: &str, cwd: &str) -> Value {
    json!({
        "hook_event_name": event,
        "session_id": session,
        "cwd": cwd,
        "transcript_path": "/dev/null",
        "permission_mode": "default",
    })
}

fn with(mut v: Value, extra: Value) -> Value {
    if let (Some(a), Some(b)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in b {
            a.insert(k.clone(), val.clone());
        }
    }
    v
}

pub const CWD: &str = "/work/payments-service";

pub fn session_start(s: &str) -> Value {
    with(base("SessionStart", s, CWD), json!({"source": "startup"}))
}

pub fn session_end(s: &str) -> Value {
    with(base("SessionEnd", s, CWD), json!({"reason": "other"}))
}

pub fn user_prompt(s: &str) -> Value {
    with(
        base("UserPromptSubmit", s, CWD),
        json!({"prompt": "PROMPT_TEXT_MUST_NOT_BE_READ"}),
    )
}

pub fn stop(s: &str, message: &str) -> Value {
    with(
        base("Stop", s, CWD),
        json!({"last_assistant_message": message, "background_tasks": [], "session_crons": [], "stop_hook_active": false}),
    )
}

pub fn stop_with_bg(s: &str, message: &str) -> Value {
    with(
        base("Stop", s, CWD),
        json!({"last_assistant_message": message, "stop_hook_active": false, "session_crons": [],
               "background_tasks": [{"id": "b1", "type": "shell", "status": "running", "description": "build", "command": "make"}]}),
    )
}

pub fn stop_failure(s: &str) -> Value {
    base("StopFailure", s, CWD)
}

pub fn pre_tool(s: &str, tool: &str, id: &str, input: Value) -> Value {
    with(
        base("PreToolUse", s, CWD),
        json!({"tool_name": tool, "tool_use_id": id, "tool_input": input}),
    )
}

pub fn post_tool(s: &str, tool: &str, id: &str, input: Value) -> Value {
    with(
        base("PostToolUse", s, CWD),
        json!({"tool_name": tool, "tool_use_id": id, "tool_input": input, "tool_response": {}}),
    )
}

pub fn permission_request(s: &str, tool: &str, input: Value) -> Value {
    with(
        base("PermissionRequest", s, CWD),
        json!({"tool_name": tool, "tool_input": input}),
    )
}

pub fn permission_prompt(s: &str) -> Value {
    with(
        base("Notification", s, CWD),
        json!({"notification_type": "permission_prompt", "message": "Claude needs your permission"}),
    )
}

pub fn ask_user_question_input(q: &str) -> Value {
    json!({"questions": [{"question": q, "header": "Q", "options": [{"label": "A", "description": "a"}, {"label": "B", "description": "b"}], "multiSelect": false}]})
}

/// Poll until `f` is true or `timeout` passes.
pub fn eventually(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}
