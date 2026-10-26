//! Hook latency (plan acceptance criterion 5):
//! `cargo test --release --test perf -- --ignored` asserts p95 < 100 ms.

mod support;

use std::time::{Duration, Instant};

use serde_json::json;
use support::*;

#[test]
#[ignore]
fn hook_p95_under_100ms() {
    let mut env = TestEnv::new(&MockNtfy::refused_url());
    env.set("TAP_TEST_CLAUDE_PID", "none");
    env.set("TAP_GRACE_OVERRIDE", "1h");
    let message = "I refactored the parser and all tests pass.\n\nShould I also update the docs?";
    let mut samples: Vec<Duration> = Vec::with_capacity(200);
    for i in 0..200 {
        let input = match i % 4 {
            0 => user_prompt("perf"),
            1 => pre_tool("perf", "Bash", &format!("t{i}"), json!({"command": "ls"})),
            2 => post_tool("perf", "Bash", &format!("t{}", i - 1), json!({})),
            _ => stop("perf", message),
        };
        let start = Instant::now();
        let out = env.hook(&input);
        samples.push(start.elapsed());
        assert!(out.status.success() && out.stdout.is_empty());
    }
    env.hook(&session_end("perf"));
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    eprintln!("hook latency: p50 {p50:?}, p95 {p95:?}");
    assert!(p95 < Duration::from_millis(100), "p95 {p95:?}");
}
