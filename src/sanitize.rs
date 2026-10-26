//! Summary sanitization (contract.md "Summary extraction"). Lane B owns this file.
//!
//! Cross-lane interface: [`summary`] and [`clean`] signatures are fixed.

use crate::contract::SUMMARY_MAX_CHARS;

/// Extract and sanitize a ≤80-char summary from an assistant message.
pub fn summary(text: &str) -> String {
    clean(
        text.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(""),
    )
}

/// Sanitize one line of untrusted text for display (no extraction).
pub fn clean(line: &str) -> String {
    let s: String = line.chars().filter(|c| !c.is_control()).collect();
    s.chars().take(SUMMARY_MAX_CHARS).collect()
}
