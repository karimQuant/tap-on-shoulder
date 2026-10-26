//! `tap doctor`: version, model variant, config validity and ignored keys,
//! ntfy reachability, open episodes, failed and uncertain deliveries, and the
//! training-file cap. Exit 1 if there is no topic, the config is invalid, or
//! the ntfy server is refused or unreachable.

use std::time::Duration;

use crate::contract::EpisodeStatus;
use crate::notify;
use crate::state::{self, Loaded, Store};

const HEAD_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run() -> i32 {
    let mut fail = false;
    print!("{}", super::version_text());

    let cwd = std::env::current_dir().ok();
    let loaded = crate::config::load(cwd.as_deref());
    match crate::config::user_config_path() {
        Some(p) if loaded.user_file => println!("config: {}", p.display()),
        Some(p) => println!("config: {} (missing, using defaults)", p.display()),
        None => println!("config: no config directory (HOME unset)"),
    }
    for e in &loaded.errors {
        println!("error: {e}");
    }
    for w in &loaded.warnings {
        println!("warning: {w}");
    }
    if !loaded.is_valid() {
        println!("config: INVALID");
        fail = true;
    }
    let c = &loaded.config;
    report_classifier(c);
    println!(
        "grace: question {}s, permission {}s, blocked {}s; alerts: completion {}, info {}",
        c.grace.question, c.grace.permission, c.grace.blocked, c.alerts_completion, c.alerts_info
    );

    let ntfy = &c.ntfy;
    match &ntfy.topic {
        None => {
            println!("ntfy: no ntfy topic configured (run /tap-on-shoulder:setup)");
            fail = true;
        }
        Some(_) => println!("ntfy: server {} (topic set)", ntfy.server),
    }
    match notify::check_server(&ntfy.server) {
        Err(e) => {
            println!("ntfy: server refused: {}", e.kind());
            fail = true;
        }
        Ok(url) => {
            let agent = notify::agent(HEAD_TIMEOUT);
            match agent.head(&url).call() {
                Ok(r) => println!("ntfy: reachable (HTTP {})", r.status().as_u16()),
                Err(_) => {
                    println!("ntfy: UNREACHABLE (taps cannot be delivered)");
                    fail = true;
                }
            }
        }
    }

    report_state();

    if crate::cli::training::at_cap() {
        println!("training: collected.jsonl reached its 10 MiB cap; collection stopped. Run `tap label`.");
    } else if c.training_collect {
        println!("training: collecting (local only)");
    }
    if fail {
        1
    } else {
        0
    }
}

/// The user's classifier settings, the classifier they select, and the
/// embedded head's decision threshold and recorded CV metrics.
fn report_classifier(c: &crate::config::Config) {
    use crate::config::{ClassifierMode, Lean};
    let mode = match c.classifier_mode {
        ClassifierMode::Model => "model",
        ClassifierMode::Rules => "rules",
    };
    let lean = match c.classifier_lean {
        Lean::Alert => "alert",
        Lean::Balanced => "balanced",
    };
    let in_use = crate::classify::load_with(c.classifier_mode, c.classifier_lean).variant();
    println!("classifier: mode {mode}, lean {lean} (in use: {in_use})");
    match serde_json::from_str::<crate::contract::Head>(crate::classify::head::HEAD_JSON) {
        Ok(h) => {
            let mut line = format!("head: {} lean_threshold {}", h.variant, h.lean_threshold);
            if let Some(r) = h.cv_tap_recall {
                line.push_str(&format!(", cv_tap_recall {r}"));
            }
            if let Some(f) = h.cv_macro_f1 {
                line.push_str(&format!(", cv_macro_f1 {f}"));
            }
            println!("{line}");
        }
        Err(_) => println!("head: unreadable"),
    }
}

fn report_state() {
    let Ok(store) = Store::open(false) else {
        println!("state: none yet");
        return;
    };
    let now = state::now();
    let (mut open, mut problems, mut unknown) = (0, 0, 0);
    for sid in store.sessions() {
        let short: String = sid.chars().take(8).collect();
        match store.load(&sid) {
            Loaded::Corrupt => {
                unknown += 1;
                println!("state: session {short}: unknown (unreadable state)");
            }
            Loaded::Missing => {}
            Loaded::Ok(s) => {
                let Some(ep) = s.episode else { continue };
                match ep.status {
                    EpisodeStatus::Open | EpisodeStatus::Sending => {
                        open += 1;
                        let due = ep.due_at.max(ep.retry_at.unwrap_or(0)) - now;
                        println!(
                            "open: {} [{}] {} due in {}s",
                            s.project,
                            short,
                            ep.category.as_str(),
                            due.max(0)
                        );
                    }
                    EpisodeStatus::Failed | EpisodeStatus::SentUncertain => {
                        problems += 1;
                        let what = if ep.status == EpisodeStatus::Failed {
                            "failed"
                        } else {
                            "sent_uncertain"
                        };
                        println!(
                            "delivery: {} [{}] {} {what} ({}) after {} attempt(s)",
                            s.project,
                            short,
                            ep.category.as_str(),
                            ep.closed_reason.as_deref().unwrap_or("-"),
                            ep.attempts
                        );
                    }
                    _ => {}
                }
            }
        }
    }
    println!("state: {open} open, {problems} delivery problem(s), {unknown} unknown");
}
