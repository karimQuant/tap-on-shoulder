//! Opt-in local training data (plan D7, R7).
//!
//! With `training.collect = true`, each `Stop` classification appends to
//! `${CLAUDE_PLUGIN_DATA}/training/collected.jsonl` (0600, `O_NOFOLLOW`,
//! capped at 10 MiB). The text is redacted and at most 4 KiB. The next
//! `UserPromptSubmit` of that session fills in `reply_after_s`.
//! `tap label` turns entries into `labeled.jsonl`.

use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::contract::{Category, Classification};
use crate::log::Entry;
use crate::state::{self, FileLock};

pub const COLLECT_CAP_BYTES: u64 = 10 << 20;
pub const TEXT_MAX_BYTES: usize = 4096;

pub fn training_dir(create: bool) -> io::Result<PathBuf> {
    let data = state::data_dir().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    if create {
        state::private_subdir(&data, "training")
    } else {
        let d = data.join("training");
        state::check_private_dir(&d)?;
        Ok(d)
    }
}

pub fn collected_path() -> Option<PathBuf> {
    state::data_dir().map(|d| d.join("training").join("collected.jsonl"))
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

/// Remove ANSI escape sequences (CSI, OSC, and two-byte escapes).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            // CSI: parameters/intermediates until a final byte in @..=~.
            Some('[') => {
                for d in it.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            // OSC: until BEL or ESC \.
            Some(']') => {
                while let Some(d) = it.next() {
                    if d == '\x07' {
                        break;
                    }
                    if d == '\x1b' {
                        it.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Remove ANSI escapes, bidi controls, and control characters other than
/// newline and tab.
pub fn strip_controls(s: &str) -> String {
    strip_ansi(s)
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !(c.is_control() || is_bidi(c)))
        .collect()
}

/// Training redaction: `sanitize::redact_for_training` (same redactions as
/// summaries, newlines kept) applied to the message ending, at most 4 KiB.
pub fn redact_for_training(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(TEXT_MAX_BYTES)..]
        .iter()
        .collect();
    let s = crate::sanitize::redact_for_training(&strip_ansi(&tail), TEXT_MAX_BYTES);
    tail_bytes(&s, TEXT_MAX_BYTES).to_string()
}

fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

fn open_append(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Append one `Stop` classification. Errors are logged by kind only.
pub fn collect(sid: &str, text: &str, c: &Classification) {
    let r = (|| -> io::Result<bool> {
        let dir = training_dir(true)?;
        let _l = FileLock::acquire(&dir.join("collect.lock"))?;
        let mut f = open_append(&dir.join("collected.jsonl"))?;
        if f.metadata()?.len() >= COLLECT_CAP_BYTES {
            return Ok(false);
        }
        let entry = json!({
            "id": state::random_hex(8),
            "ts": state::now(),
            "session": sid,
            "text": redact_for_training(text),
            "category": c.category.as_str(),
            "probs": c.probs,
            "reply_after_s": Value::Null,
        });
        let mut line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        line.push(b'\n');
        f.write_all(&line)?;
        Ok(true)
    })();
    match r {
        Ok(true) => {}
        Ok(false) => Entry::new("Stop").session(sid).kind("training_cap").write(),
        Err(_) => Entry::new("Stop")
            .session(sid)
            .kind("training_write")
            .write(),
    }
}

/// Fill `reply_after_s` on the most recent entry of `sid`, if still empty.
pub fn record_reply(sid: &str) {
    let r = (|| -> io::Result<()> {
        let dir = training_dir(false)?;
        let _l = FileLock::acquire(&dir.join("collect.lock"))?;
        let path = dir.join("collected.jsonl");
        let bytes = match state::read_nofollow(&path, COLLECT_CAP_BYTES + (1 << 20)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            r => r?,
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let Some((idx, mut v)) = lines.iter().enumerate().rev().find_map(|(i, l)| {
            let v: Value = serde_json::from_str(l).ok()?;
            (v["session"] == sid).then_some((i, v))
        }) else {
            return Ok(());
        };
        if !v["reply_after_s"].is_null() {
            return Ok(());
        }
        let ts = v["ts"].as_i64().unwrap_or_else(state::now);
        v["reply_after_s"] = json!((state::now() - ts).max(0));
        lines[idx] = v.to_string();
        let mut out = lines.join("\n");
        out.push('\n');
        state::write_atomic(&path, out.as_bytes())
    })();
    if r.is_err() {
        Entry::new("UserPromptSubmit")
            .session(sid)
            .kind("training_reply")
            .write();
    }
}

/// Whether the collection file has reached its cap.
pub fn at_cap() -> bool {
    collected_path()
        .and_then(|p| std::fs::symlink_metadata(p).ok())
        .is_some_and(|m| m.len() >= COLLECT_CAP_BYTES)
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    match state::read_nofollow(path, COLLECT_CAP_BYTES + (1 << 20)) {
        Ok(b) => String::from_utf8_lossy(&b)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// `tap label`: interactive labelling loop. Returns the process exit code.
pub fn label(input: &mut dyn BufRead, out: &mut dyn Write) -> i32 {
    let Ok(dir) = training_dir(false) else {
        let _ = writeln!(out, "no training data: set training.collect = true first");
        return 1;
    };
    let collected = read_jsonl(&dir.join("collected.jsonl"));
    let labeled_path = dir.join("labeled.jsonl");
    let done: std::collections::HashSet<String> = read_jsonl(&labeled_path)
        .iter()
        .filter_map(|v| v["id"].as_str().map(String::from))
        .collect();
    let todo: Vec<&Value> = collected
        .iter()
        .filter(|v| v["id"].as_str().is_some_and(|id| !done.contains(id)))
        .collect();
    if todo.is_empty() {
        let _ = writeln!(out, "nothing to label");
        return 0;
    }
    let mut labeled = 0;
    for (i, v) in todo.iter().enumerate() {
        let text = strip_controls(v["text"].as_str().unwrap_or(""));
        let predicted = strip_controls(v["category"].as_str().unwrap_or("?"));
        let _ = write!(
            out,
            "\n--- {}/{} (predicted: {predicted}) ---\n{text}\n\n1) needs_decision  2) blocked  3) done  4) info  s) skip  q) quit\n> ",
            i + 1,
            todo.len()
        );
        let _ = out.flush();
        let mut line = String::new();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let label = match line.trim() {
            "1" => Category::NeedsDecision,
            "2" => Category::Blocked,
            "3" => Category::Done,
            "4" => Category::Info,
            "q" => break,
            _ => continue,
        };
        let entry = json!({
            "id": v["id"],
            "text": v["text"],
            "label": label.as_str(),
            "predicted": v["category"],
            "ts": v["ts"],
        });
        let r = open_append(&labeled_path).and_then(|mut f| {
            let mut l = entry.to_string().into_bytes();
            l.push(b'\n');
            f.write_all(&l)
        });
        if r.is_err() {
            let _ = writeln!(out, "could not write labeled.jsonl");
            return 1;
        }
        labeled += 1;
    }
    let _ = writeln!(out, "\nlabeled {labeled} entries");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_keeps_newlines_and_removes_secrets() {
        let t = "Mail bob@example.com\nsee /home/u/secret/file.rs and https://x.y/z\nkey sk-abcdefghijklmnop\n\x1b[31mred\x1b[0m\u{202E}x";
        let r = redact_for_training(t);
        assert!(r.contains('\n'));
        assert!(!r.contains("bob@"));
        assert!(!r.contains("/home/u"));
        assert!(!r.contains("x.y"));
        assert!(!r.contains("sk-abc"));
        assert!(!r.contains('\x1b'));
        assert!(!r.contains('\u{202E}'));
        assert!(r.contains("<email>") && r.contains("<path>") && r.contains("<url>"));
    }

    #[test]
    fn tail_is_capped_on_char_boundary() {
        let s = "é".repeat(5000);
        let r = redact_for_training(&s);
        assert!(r.len() <= TEXT_MAX_BYTES);
        assert!(r.chars().all(|c| c == 'é'));
    }
}
