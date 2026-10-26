//! User-facing commands: `setup`, `test`, `doctor`, `label`, `--version`.

pub mod doctor;
pub mod setup;
pub mod training;

use crate::contract::{Notifier, Tap, FEATURE_SPEC};
use crate::notify::NtfyNotifier;

/// `tap --version`.
pub fn version_text() -> String {
    let variant = crate::classify::load().variant();
    format!(
        "tap {}\nmodel: {variant}\nfeature_spec: {FEATURE_SPEC}\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn cwd_project() -> (Option<std::path::PathBuf>, String) {
    let cwd = std::env::current_dir().ok();
    let name = cwd
        .as_ref()
        .and_then(|c| c.file_name())
        .map(|n| crate::sanitize::clean(&n.to_string_lossy()))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "tap-on-shoulder".into());
    (cwd, name)
}

/// `tap test`: send one test tap. Returns the exit code.
pub fn test_tap() -> i32 {
    let (cwd, project) = cwd_project();
    let loaded = crate::config::load(cwd.as_deref());
    let notifier = match NtfyNotifier::new(&loaded.config.ntfy) {
        Ok(n) => n,
        Err(e) => {
            eprintln!(
                "cannot send: {} (run /tap-on-shoulder:setup or fix the config)",
                e.kind()
            );
            return 1;
        }
    };
    let tap = Tap {
        title: format!("{project} · test tap"),
        message: "tap-on-shoulder is set up. This is a test.".into(),
        priority: 3,
        tags: vec!["test".into()],
    };
    match notifier.send(&tap) {
        Ok(()) => {
            println!("test tap sent");
            0
        }
        Err(e) => {
            eprintln!("test tap failed: {}", crate::notify::error_kind(&e));
            1
        }
    }
}

/// `tap label`.
pub fn label() -> i32 {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    training::label(&mut input, &mut out)
}

/// `tap classify`: classify a message read from stdin and print the category,
/// the sanitized summary, and the model probabilities. Diagnostic only: no
/// state is written and nothing is sent.
pub fn classify_stdin() -> i32 {
    use std::io::Read;
    let mut buf = Vec::new();
    if std::io::stdin()
        .lock()
        .take(crate::contract::STDIN_MAX_BYTES as u64)
        .read_to_end(&mut buf)
        .is_err()
    {
        eprintln!("tap classify: cannot read stdin");
        return 2;
    }
    let text = String::from_utf8_lossy(&buf);
    let classifier = crate::classify::load();
    let c = classifier.classify(&text);
    println!("category: {}", c.category.as_str());
    println!("summary:  {}", c.summary);
    match c.probs {
        Some(p) => println!(
            "probs:    needs_decision={:.3} blocked={:.3} done={:.3} info={:.3} ({})",
            p[0],
            p[1],
            p[2],
            p[3],
            classifier.variant()
        ),
        None => println!("probs:    - ({})", classifier.variant()),
    }
    0
}
