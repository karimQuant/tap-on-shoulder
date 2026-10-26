//! Summary sanitization (contract.md "Summary extraction"). Lane B owns this file.
//!
//! Cross-lane interface: [`summary`] and [`clean`] signatures are fixed.
//! [`redact_for_training`] applies the same redactions but keeps newlines.
//!
//! Every input here is untrusted. Output never contains control or bidi
//! characters, URLs, emails, absolute paths, or key-like tokens.
//!
//! The redaction patterns are the contract regexes, implemented as linear
//! hand scanners: compiling Unicode-aware `regex` classes costs several ms,
//! and every hook runs in a fresh process.

use crate::classify::features::{is_blocker, is_error, prepare_text};
use crate::classify::wordpiece::{is_clean_drop, is_nonspacing_mark};
use crate::contract::{SUMMARY_MAX_CHARS, TEXT_MAX_CHARS};

/// Extract and sanitize a ≤80-char summary from an assistant message.
///
/// Candidates, scanned from the end of the text: the last line containing
/// `?`, else the last line matching the blocker or error features, else the
/// last non-empty line.
pub fn summary(text: &str) -> String {
    let text = prepare_text(text);
    let lines: Vec<&str> = text.lines().collect();
    let pick = lines
        .iter()
        .rev()
        .find(|l| l.contains('?'))
        .or_else(|| lines.iter().rev().find(|l| is_blocker(l) || is_error(l)))
        .or_else(|| lines.iter().rev().find(|l| !l.trim().is_empty()))
        .copied()
        .unwrap_or("");
    clean(&strip_markdown(pick))
}

/// Sanitize one line of untrusted text for display (no extraction).
pub fn clean(line: &str) -> String {
    let s = strip_invisible(cap(line, TEXT_MAX_CHARS), false);
    let s = redact(&s);
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&s, SUMMARY_MAX_CHARS)
}

/// Redact text for the opt-in training log: same redactions as [`clean`],
/// newlines kept, runs of other whitespace collapsed, at most `max_chars`.
pub fn redact_for_training(text: &str, max_chars: usize) -> String {
    let s = strip_invisible(cap(text, TEXT_MAX_CHARS.max(max_chars)), true);
    let s = redact(&s);
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if c != '\n' && c.is_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out.chars().take(max_chars).collect()
}

/// Apply, in order: URLs → `<url>`, emails → `<email>`, absolute paths →
/// `<path>` (leading whitespace kept), key-like and 32+ char tokens →
/// `<redacted>`.
fn redact(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let c = scan(&c, "<url>", url_at);
    let c = scan(&c, "<email>", email_at);
    let c = scan(&c, "<path>", unix_path_at);
    let c = scan(&c, "<path>", win_path_at);
    let c = scan(&c, "<redacted>", key_at);
    let c = scan(&c, "<redacted>", long_token_at);
    c.into_iter().collect()
}

/// Leftmost, non-overlapping replace. `find(chars, i, floor)` returns the
/// match `[start, end)` triggered at `i`, with `floor <= start <= i < end`.
fn scan(
    chars: &[char],
    rep: &str,
    find: impl Fn(&[char], usize, usize) -> Option<(usize, usize)>,
) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let (mut emitted, mut i) = (0, 0);
    while i < chars.len() {
        match find(chars, i, emitted) {
            Some((start, end)) => {
                out.extend_from_slice(&chars[emitted..start]);
                out.extend(rep.chars());
                emitted = end;
                i = end;
            }
            None => i += 1,
        }
    }
    out.extend_from_slice(&chars[emitted..]);
    out
}

/// Regex `\w` (Unicode word character).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || is_nonspacing_mark(c)
}

fn run(chars: &[char], from: usize, class: impl Fn(char) -> bool) -> usize {
    let mut j = from;
    while j < chars.len() && class(chars[j]) {
        j += 1;
    }
    j
}

fn starts_with_ci(chars: &[char], i: usize, lit: &str) -> bool {
    let mut k = i;
    for l in lit.chars() {
        match chars.get(k) {
            Some(c) if c.to_ascii_lowercase() == l => k += 1,
            _ => return false,
        }
    }
    true
}

/// `(?i)https?://\S+`
fn url_at(c: &[char], i: usize, _: usize) -> Option<(usize, usize)> {
    let n = ["https://", "http://"]
        .iter()
        .find(|p| starts_with_ci(c, i, p))?
        .len();
    let end = run(c, i + n, |x| !x.is_whitespace());
    (end > i + n).then_some((i, end))
}

/// `[\w.+\-]+@[\w\-]+(?:\.[\w\-]+)+`
fn email_at(c: &[char], i: usize, floor: usize) -> Option<(usize, usize)> {
    if c[i] != '@' {
        return None;
    }
    let local = |x: char| is_word(x) || matches!(x, '.' | '+' | '-');
    let label = |x: char| is_word(x) || x == '-';
    let mut start = i;
    while start > floor && local(c[start - 1]) {
        start -= 1;
    }
    let mut end = run(c, i + 1, label);
    if start == i || end == i + 1 {
        return None;
    }
    let mut dots = 0;
    while end < c.len() && c[end] == '.' {
        let next = run(c, end + 1, label);
        if next == end + 1 {
            break;
        }
        end = next;
        dots += 1;
    }
    (dots > 0).then_some((start, end))
}

/// `(^|\s)(~?/[\w.\-]+(?:/[\w.\-]+)+)`, replacing only group 2.
fn unix_path_at(c: &[char], i: usize, _: usize) -> Option<(usize, usize)> {
    if i > 0 && !c[i - 1].is_whitespace() {
        return None;
    }
    let seg = |x: char| is_word(x) || matches!(x, '.' | '-');
    let mut j = i;
    if c.get(j) == Some(&'~') {
        j += 1;
    }
    let mut segments = 0;
    while c.get(j) == Some(&'/') {
        let next = run(c, j + 1, seg);
        if next == j + 1 {
            break;
        }
        j = next;
        segments += 1;
    }
    (segments >= 2).then_some((i, j))
}

/// `[A-Za-z]:\\\S+`
fn win_path_at(c: &[char], i: usize, _: usize) -> Option<(usize, usize)> {
    if !(c[i].is_ascii_alphabetic() && c.get(i + 1) == Some(&':') && c.get(i + 2) == Some(&'\\')) {
        return None;
    }
    let end = run(c, i + 3, |x| !x.is_whitespace());
    (end > i + 3).then_some((i, end))
}

/// `(?:sk|pk|ghp|gho|ghs|github_pat|xox[abpr]|AKIA)[\w\-]{8,}`
fn key_at(c: &[char], i: usize, _: usize) -> Option<(usize, usize)> {
    const PREFIXES: [&str; 11] = [
        "sk",
        "pk",
        "ghp",
        "gho",
        "ghs",
        "github_pat",
        "xoxa",
        "xoxb",
        "xoxp",
        "xoxr",
        "AKIA",
    ];
    let starts = |p: &str| p.chars().enumerate().all(|(k, l)| c.get(i + k) == Some(&l));
    let n = PREFIXES.iter().find(|p| starts(p))?.len();
    let end = run(c, i + n, |x| is_word(x) || x == '-');
    (end - (i + n) >= 8).then_some((i, end))
}

/// `[A-Za-z0-9+/=_\-]{32,}`
fn long_token_at(c: &[char], i: usize, _: usize) -> Option<(usize, usize)> {
    let end = run(c, i, |x| x.is_ascii_alphanumeric() || "+/=_-".contains(x));
    (end - i >= 32).then_some((i, end))
}

/// First `max` scalar values. If cut, the trailing partial token is dropped
/// too, so a secret split by the cut cannot slip past the redaction regexes.
fn cap(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        None => s,
        Some((i, _)) => {
            let head = &s[..i];
            match head.rfind(char::is_whitespace) {
                Some(j) => &head[..j],
                None => "",
            }
        }
    }
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

/// Drop control, bidi, and other `C*` characters; whitespace becomes a space
/// (or stays `\n` when `keep_newlines`).
fn strip_invisible(s: &str, keep_newlines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\n' && keep_newlines {
            out.push('\n');
        } else if c.is_whitespace() {
            out.push(' ');
        } else if !(c.is_control() || is_bidi(c) || is_clean_drop(c)) {
            out.push(c);
        }
    }
    out
}

/// Remove markdown markers: leading `#`/`>`/list bullets, every `*` and
/// `` ` ``, and `_` runs not joining two alphanumerics (keeps `snake_case`).
fn strip_markdown(line: &str) -> String {
    let mut s = line.trim_start();
    loop {
        let t = s.trim_start_matches(['#', '>']).trim_start();
        if t.len() == s.len() {
            break;
        }
        s = t;
    }
    let s = strip_bullet(s);
    let chars: Vec<char> = s.chars().filter(|c| *c != '*' && *c != '`').collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '_' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == '_' {
            i += 1;
        }
        let joins = start > 0
            && chars[start - 1].is_alphanumeric()
            && i < chars.len()
            && chars[i].is_alphanumeric();
        if joins {
            out.extend(&chars[start..i]);
        }
    }
    out
}

/// `^(?:[-*+•]|\d+[.)])\s+`
fn strip_bullet(s: &str) -> &str {
    let rest = if let Some(r) = s.strip_prefix(['-', '*', '+', '•']) {
        r
    } else {
        let r = s.trim_start_matches(|c: char| c.is_ascii_digit());
        match r.strip_prefix(['.', ')']) {
            Some(r2) if r.len() < s.len() => r2,
            _ => return s,
        }
    };
    let t = rest.trim_start();
    if t.len() < rest.len() {
        t
    } else {
        s
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_question_then_blocker_then_last_line() {
        assert_eq!(
            summary("Plan:\n- step\n\n**Should I proceed?**\nThanks."),
            "Should I proceed?"
        );
        assert_eq!(
            summary("Tried twice.\nBuild failed to link.\nStopping here."),
            "Build failed to link."
        );
        assert_eq!(summary("one\n\n## Two done\n\n  \n"), "Two done");
        assert_eq!(summary(""), "");
    }

    #[test]
    fn strips_markdown_markers() {
        assert_eq!(
            strip_markdown("> ## 1. Use `foo_bar` _now_"),
            "Use foo_bar now"
        );
        assert_eq!(strip_markdown("- **bold** item"), "bold item");
    }

    #[test]
    fn truncates_to_80() {
        let s = clean(&"word ".repeat(200));
        assert_eq!(s.chars().count(), 80);
        assert!(s.ends_with('…'));
        assert_eq!(clean("short"), "short");
    }
}
