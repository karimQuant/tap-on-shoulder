//! Hand features f0–f11 (`feature_spec = 1`, contract.md "Hand features").

use crate::contract::{N_FEATURES, TAIL_CHARS, TEXT_MAX_CHARS};

pub const F_LAST_LINE_QUESTION: usize = 0;
pub const F_HAS_QUESTION: usize = 1;
pub const F_OFFER: usize = 2;
pub const F_DECISION: usize = 3;
pub const F_BLOCKER: usize = 4;
pub const F_COMPLETION: usize = 5;
pub const F_CODE_RATIO: usize = 6;
pub const F_LENGTH: usize = 7;
pub const F_OPTIONS_LIST: usize = 8;
pub const F_ERROR_MARKER: usize = 9;
pub const F_QUESTION_COUNT: usize = 10;
pub const F_IMPERATIVE: usize = 11;

pub type Features = [f32; N_FEATURES];

// The contract regexes are alternations of literal phrases, so they are
// matched here as expanded phrase lists: `(bounded, text)`, where `bounded`
// means the alternative is wrapped in `\b…\b`. This has no compile cost (each
// hook is a fresh process); the test module cross-checks it against `regex`.
type Phrase = (bool, &'static str);

const OFFER: &[Phrase] = &[
    (true, "let me know if"),
    (true, "would you like me to"),
    (true, "want me to"),
    (true, "if you'd like"),
    (true, "if you would like"),
    (true, "happy to"),
];
const DECISION: &[Phrase] = &[
    (true, "should i"),
    (true, "which option"),
    (true, "which one"),
    (true, "which approach"),
    (true, "do you want"),
    (true, "would you prefer"),
    (true, "please confirm"),
    (true, "please choose"),
    (true, "please decide"),
    (true, "before i proceed"),
    (true, "your call"),
    (true, "need your input"),
    (true, "need your decision"),
];
const BLOCKER: &[Phrase] = &[
    (true, "permission denied"),
    (true, "missing"),
    (true, "not found"),
    (true, "failed to"),
    (true, "unable to"),
    (true, "can't continue"),
    (true, "cannot continue"),
    (true, "blocked"),
    (true, "need you to"),
    (true, "credential"),
    (true, "credentials"),
    (true, "api key"),
    (true, "unauthorised"),
    (true, "unauthorized"),
    (true, "forbidden"),
    (true, "rate limit"),
];
const COMPLETION: &[Phrase] = &[
    (false, "all tests pass"),
    (true, "done"),
    (true, "complete"),
    (true, "completed"),
    (true, "finished"),
    (false, "✅"),
    (false, "successfully"),
    (true, "merged"),
    (true, "fixed"),
];
const ERROR: &[Phrase] = &[
    (true, "error"),
    (false, "exception"),
    (false, "traceback"),
    (false, "exit code 1"),
    (false, "exit code 2"),
    (false, "exit code 3"),
    (false, "exit code 4"),
    (false, "exit code 5"),
    (false, "exit code 6"),
    (false, "exit code 7"),
    (false, "exit code 8"),
    (false, "exit code 9"),
];
const IMPERATIVE: &[Phrase] = &[
    (true, "please run"),
    (true, "please provide"),
    (true, "please set"),
    (true, "please add"),
    (true, "please check"),
    (true, "you'll need to"),
    (true, "you will need to"),
    (true, "you need to"),
];

/// Word character for `\b`: alphanumeric or `_`.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One-char case fold for `(?i)`. Python `re.IGNORECASE` also folds
/// U+0130, U+0131, U+017F, and U+212A onto ASCII letters.
fn fold(c: char) -> char {
    match c {
        '\u{130}' | '\u{131}' => 'i',
        '\u{17F}' => 's',
        '\u{212A}' => 'k',
        _ => c.to_lowercase().next().unwrap_or(c),
    }
}

/// Text prepared for case-insensitive phrase search.
struct Folded {
    orig: Vec<char>,
    low: Vec<char>,
}

impl Folded {
    fn new(s: &str) -> Self {
        let orig: Vec<char> = s.chars().collect();
        let low = orig.iter().map(|&c| fold(c)).collect();
        Folded { orig, low }
    }

    fn any(&self, phrases: &[Phrase]) -> bool {
        phrases.iter().any(|&(b, p)| self.has(p, b))
    }

    fn has(&self, phrase: &str, bounded: bool) -> bool {
        let mut buf = ['\0'; 32];
        let mut len = 0;
        for c in phrase.chars() {
            buf[len] = c;
            len += 1;
        }
        let p = &buf[..len];
        let n = self.low.len();
        if p.len() > n {
            return false;
        }
        (0..=n - p.len()).any(|i| {
            self.low[i] == p[0]
                && self.low[i..i + p.len()] == *p
                && (!bounded
                    || ((i == 0 || !is_word(self.orig[i - 1]))
                        && (i + p.len() == n || !is_word(self.orig[i + p.len()]))))
        })
    }
}

/// f4 `blocker` (shared with summary extraction).
pub fn is_blocker(s: &str) -> bool {
    Folded::new(s).any(BLOCKER)
}

/// f9 `error_marker` (shared with summary extraction).
pub fn is_error(s: &str) -> bool {
    Folded::new(s).any(ERROR)
}

/// f8 line test: `(?i)^\s*(\d+[.)]|[-*•]|\(?[a-d][.)])\s`.
fn is_option_line(line: &str) -> bool {
    let c: Vec<char> = line.trim_start().chars().collect();
    let ws = |i: usize| c.get(i).is_some_and(|x| x.is_whitespace());
    let close = |i: usize| matches!(c.get(i), Some('.' | ')'));
    let letter = |i: usize| c.get(i).is_some_and(|x| matches!(fold(*x), 'a'..='d'));
    let digits = c.iter().take_while(|x| x.is_ascii_digit()).count();
    (digits > 0 && close(digits) && ws(digits + 1))
        || (matches!(c.first(), Some('-' | '*' | '•')) && ws(1))
        || (c.first() == Some(&'(') && letter(1) && close(2) && ws(3))
        || (letter(0) && close(1) && ws(2))
}

/// `text`: the first 16 384 scalar values of the message.
pub fn prepare_text(message: &str) -> &str {
    match message.char_indices().nth(TEXT_MAX_CHARS) {
        Some((i, _)) => &message[..i],
        None => message,
    }
}

/// `tail`: the last 600 scalar values of `text.trim_end()`.
pub fn tail_of(text: &str) -> &str {
    let t = text.trim_end();
    let n = t.chars().count();
    if n <= TAIL_CHARS {
        return t;
    }
    let skip = n - TAIL_CHARS;
    let (i, _) = t.char_indices().nth(skip).expect("index in range");
    &t[i..]
}

fn b(x: bool) -> f32 {
    if x {
        1.0
    } else {
        0.0
    }
}

/// f0: last line whose `trim()` is non-empty, trailing whitespace and
/// `` *_`)"' `` trimmed, ends with `?`.
fn last_line_question(tail: &str) -> bool {
    tail.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            l.trim_end_matches(|c: char| c.is_whitespace() || "*_`)\"'".contains(c))
                .ends_with('?')
        })
        .unwrap_or(false)
}

/// f6: chars in odd-indexed segments of `text.split("```")` over `len(text)`.
fn code_ratio(text: &str) -> f32 {
    let total = text.chars().count();
    if total == 0 {
        return 0.0;
    }
    let inside: usize = text
        .split("```")
        .skip(1)
        .step_by(2)
        .map(|s| s.chars().count())
        .sum();
    inside as f32 / total as f32
}

/// Features of a message, given its prepared `text` (see [`prepare_text`]).
pub fn compute(text: &str) -> Features {
    let tail = tail_of(text);
    let folded = Folded::new(tail);
    let len = text.chars().count();
    let qs = tail.matches('?').count();
    let has_q = qs > 0;
    let option_lines = tail.lines().filter(|l| is_option_line(l)).count();
    let mut f = [0f32; N_FEATURES];
    f[F_LAST_LINE_QUESTION] = b(last_line_question(tail));
    f[F_HAS_QUESTION] = b(has_q);
    f[F_OFFER] = b(folded.any(OFFER));
    f[F_DECISION] = b(folded.any(DECISION));
    f[F_BLOCKER] = b(folded.any(BLOCKER));
    f[F_COMPLETION] = b(folded.any(COMPLETION));
    f[F_CODE_RATIO] = code_ratio(text);
    f[F_LENGTH] = ((1.0 + len as f64).ln() / (1.0 + TEXT_MAX_CHARS as f64).ln()) as f32;
    f[F_OPTIONS_LIST] = b(option_lines >= 2 && has_q);
    f[F_ERROR_MARKER] = b(folded.any(ERROR));
    f[F_QUESTION_COUNT] = qs.min(5) as f32 / 5.0;
    f[F_IMPERATIVE] = b(folded.any(IMPERATIVE));
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    /// The contract regexes verbatim (reference implementation).
    fn reference(tail: &str) -> [bool; 7] {
        static RES: std::sync::OnceLock<Vec<Regex>> = std::sync::OnceLock::new();
        let res = RES.get_or_init(|| {
            [
                r"\b(let me know if|would you like me to|want me to|if you'd like|if you would like|happy to)\b",
                r"\b(should i|which (option|one|approach)|do you want|would you prefer|please (confirm|choose|decide)|before i proceed|your call|need your (input|decision))\b",
                r"\b(permission denied|missing|not found|failed to|unable to|can't continue|cannot continue|blocked|need you to|credentials?|api key|unauthori[sz]ed|forbidden|rate limit)\b",
                r"(all tests pass|\bdone\b|\bcompleted?\b|\bfinished\b|✅|successfully|\bmerged\b|\bfixed\b)",
                r"(\berror\b|exception|traceback|exit code [1-9])",
                r"\b(please (run|provide|set|add|check)|you'll need to|you will need to|you need to)\b",
                r"^\s*(\d+[.)]|[-*•]|\(?[a-d][.)])\s",
            ]
            .iter()
            .map(|p| Regex::new(&format!("(?i){p}")).unwrap())
            .collect()
        });
        let mut out = [false; 7];
        for (o, r) in out.iter_mut().zip(&res[..6]) {
            *o = r.is_match(tail);
        }
        out[6] = tail.lines().filter(|l| res[6].is_match(l)).count() >= 2;
        out
    }

    fn hand(tail: &str) -> [bool; 7] {
        let f = Folded::new(tail);
        [
            f.any(OFFER),
            f.any(DECISION),
            f.any(BLOCKER),
            f.any(COMPLETION),
            f.any(ERROR),
            f.any(IMPERATIVE),
            tail.lines().filter(|l| is_option_line(l)).count() >= 2,
        ]
    }

    #[test]
    fn hand_matchers_equal_contract_regexes() {
        let words = [
            "Should",
            "I",
            "which",
            "option",
            "one",
            "approach",
            "do",
            "you",
            "want",
            "me",
            "to",
            "let",
            "know",
            "if",
            "would",
            "like",
            "prefer",
            "please",
            "confirm",
            "run",
            "check",
            "missing",
            "credentials",
            "credential",
            "API",
            "key",
            "unauthorised",
            "Blocked",
            "done",
            "completed",
            "complete",
            "finished",
            "✅",
            "successfully",
            "merged",
            "fixed",
            "error",
            "errors",
            "exception",
            "Traceback",
            "exit",
            "code",
            "1",
            "0",
            "can't",
            "continue",
            "you'll",
            "need",
            "your",
            "call",
            "input",
            "happy",
            "rate",
            "limit",
            "not",
            "found",
            "failed",
            "unable",
            "before",
            "proceed",
            "all",
            "tests",
            "pass",
            "passed",
            "1.",
            "2)",
            "(a)",
            "b.",
            "-",
            "*",
            "•",
            "e.",
            "x_done",
            "done_y",
            "é",
            "Dône",
            "forbidden",
            "permission",
            "denied",
            "id",
            "like",
            "MISSING",
            "Fixed!",
        ];
        let seps = [
            " ", " ", " ", "\n", "\n  ", ", ", "_", "", ".", "?", "-", "'",
        ];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |m: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % m as u64) as usize
        };
        for _ in 0..20_000 {
            let mut s = String::new();
            for _ in 0..1 + next(12) {
                s.push_str(words[next(words.len())]);
                s.push_str(seps[next(seps.len())]);
            }
            assert_eq!(hand(&s), reference(&s), "{s:?}");
        }
    }

    #[test]
    fn question_features() {
        let f = compute("Here is the plan.\n\nShould I proceed?**\n\n");
        assert_eq!(f[F_LAST_LINE_QUESTION], 1.0);
        assert_eq!(f[F_DECISION], 1.0);
        assert_eq!(f[F_QUESTION_COUNT], 0.2);
        let f = compute("Is this ok? Yes.\nDone.");
        assert_eq!(f[F_LAST_LINE_QUESTION], 0.0);
        assert_eq!(f[F_HAS_QUESTION], 1.0);
        assert_eq!(f[F_COMPLETION], 1.0);
    }

    #[test]
    fn options_list_needs_question() {
        let f = compute("Pick one?\n1. alpha\n2) beta\n(c) gamma");
        assert_eq!(f[F_OPTIONS_LIST], 1.0);
        let f = compute("Steps:\n1. alpha\n2. beta");
        assert_eq!(f[F_OPTIONS_LIST], 0.0);
    }

    #[test]
    fn code_ratio_and_length() {
        assert_eq!(compute("")[F_CODE_RATIO], 0.0);
        assert_eq!(compute("")[F_LENGTH], 0.0);
        // "ab```cd```ef" → inside "cd": 2 / 12.
        assert!((compute("ab```cd```ef")[F_CODE_RATIO] - 2.0 / 12.0).abs() < 1e-6);
        // Unclosed fence runs to the end: "xyz" = 3 / 8.
        assert!((compute("ab```xyz")[F_CODE_RATIO] - 3.0 / 8.0).abs() < 1e-6);
        let full = "a".repeat(TEXT_MAX_CHARS);
        assert!((compute(&full)[F_LENGTH] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn tail_and_text_limits() {
        let long = "é".repeat(20_000);
        assert_eq!(prepare_text(&long).chars().count(), TEXT_MAX_CHARS);
        assert_eq!(
            tail_of(&format!("{}x   \n", "y".repeat(700)))
                .chars()
                .count(),
            600
        );
    }

    #[test]
    fn blocker_error_imperative_case_insensitive() {
        let f = compute("PERMISSION DENIED. Traceback follows. Please RUN the setup.");
        assert_eq!(f[F_BLOCKER], 1.0);
        assert_eq!(f[F_ERROR_MARKER], 1.0);
        assert_eq!(f[F_IMPERATIVE], 1.0);
        assert_eq!(compute("Would you like me to add tests?")[F_OFFER], 1.0);
    }
}
