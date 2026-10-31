//! `tap setup [--force] [--server URL]`: write the user config (0600) with a
//! random topic and print the subscribe URL.

use std::os::unix::fs::DirBuilderExt;

use crate::config::{self, DEFAULT_SERVER};
use crate::notify;

const TOPIC_RANDOM_CHARS: usize = 24;
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// `tap-` followed by 24 random lowercase alphanumerics, or `None` if the
/// OS random source fails. Never a shorter (guessable) topic.
pub fn random_topic() -> Option<String> {
    let mut out = String::from("tap-");
    while out.len() < 4 + TOPIC_RANDOM_CHARS {
        let mut buf = [0u8; 32];
        fill_random(&mut buf).ok()?;
        for b in buf {
            // Rejection sampling keeps the distribution uniform.
            if (b as usize) < 252 && out.len() < 4 + TOPIC_RANDOM_CHARS {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    Some(out)
}

/// `getrandom`, failing on demand under `TAP_TEST_GETRANDOM_FAIL=1` (debug
/// builds with `TAP_DEV=1` only).
fn fill_random(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    if config::test_knobs() && std::env::var("TAP_TEST_GETRANDOM_FAIL").as_deref() == Ok("1") {
        return Err(getrandom::Error::UNSUPPORTED);
    }
    getrandom::fill(buf)
}

/// Exactly `tap-` plus 24 characters of the topic alphabet.
fn topic_is_full(t: &str) -> bool {
    t.len() == 4 + TOPIC_RANDOM_CHARS
        && t.starts_with("tap-")
        && t[4..].bytes().all(|b| ALPHABET.contains(&b))
}

pub fn config_text(server: &str, topic: &str) -> String {
    format!(
        "# tap-on-shoulder user config. See docs/configuration.md.\n\
         [notify.ntfy]\n\
         server = \"{server}\"\n\
         topic = \"{topic}\"\n\
         privacy = \"summary\"\n\
         # token_env = \"TAP_NTFY_TOKEN\"\n\
         \n\
         [grace]\n\
         question = \"5m\"\n\
         permission = \"5m\"\n\
         blocked = \"5m\"\n\
         \n\
         [alerts]\n\
         completion = false\n\
         info = false\n\
         \n\
         [training]\n\
         collect = false\n\
         \n\
         # Local idle ledger (stats/waits.jsonl): timings only, no message text.\n\
         [stats]\n\
         enabled = true\n"
    )
}

pub fn run(args: &[String]) -> i32 {
    let mut force = false;
    let mut server = DEFAULT_SERVER.to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" => force = true,
            "--server" => match it.next() {
                Some(s) => server = s.trim_end_matches('/').to_string(),
                None => {
                    eprintln!("--server needs a URL");
                    return 2;
                }
            },
            _ => {
                eprintln!("usage: tap setup [--force] [--server URL]");
                return 2;
            }
        }
    }
    if server
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_whitespace() || c.is_control())
    {
        eprintln!("invalid server URL");
        return 2;
    }
    if let Err(e) = notify::check_server(&server) {
        eprintln!("server refused: {}", e.kind());
        return 2;
    }
    let Some(path) = config::user_config_path() else {
        eprintln!("cannot locate the config directory (set HOME or XDG_CONFIG_HOME)");
        return 1;
    };
    if path.exists() && !force {
        eprintln!(
            "config already exists at {}; not overwriting (use --force)",
            path.display()
        );
        let loaded = config::load(None);
        if let Some(topic) = loaded.config.ntfy.topic {
            println!("subscribe: {}/{}", loaded.config.ntfy.server, topic);
        }
        print_ledger_note(loaded.config.stats_enabled);
        return 0;
    }
    let dir = path.parent().expect("config path has a parent");
    if let Some(base) = dir.parent() {
        if std::fs::create_dir_all(base).is_err() {
            eprintln!("cannot create {}", base.display());
            return 1;
        }
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            eprintln!("cannot create {}", dir.display());
            return 1;
        }
    }
    let topic = match random_topic() {
        Some(t) if topic_is_full(&t) => t,
        _ => {
            eprintln!("cannot generate a random topic: the OS random source failed");
            return 1;
        }
    };
    if crate::state::write_atomic(&path, config_text(&server, &topic).as_bytes()).is_err() {
        eprintln!("cannot write {}", path.display());
        return 1;
    }
    println!("wrote {}", path.display());
    println!("subscribe: {server}/{topic}");
    println!("Open that URL in the ntfy app (or browser), then run `tap test`.");
    print_ledger_note(true);
    0
}

/// Setup tells the user about the idle ledger and its off switch.
fn print_ledger_note(enabled: bool) {
    let path = crate::stats::ledger_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "${CLAUDE_PLUGIN_DATA}/stats/waits.jsonl".into());
    if enabled {
        println!(
            "idle ledger: on; records how long agents waited on you at {path} (local only, no message text; see `tap stats`); {}",
            crate::stats::OFF_HINT
        );
    } else {
        println!("idle ledger: off; {}", crate::stats::ON_HINT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_shape() {
        let t = random_topic().unwrap();
        assert_eq!(t.len(), 28);
        assert!(topic_is_full(&t));
        assert!(config::valid_topic(&t));
        assert_ne!(Some(t), random_topic());
        assert!(!topic_is_full("tap-abc"));
    }

    #[test]
    fn generated_config_parses() {
        let text = config_text("https://ntfy.sh", "tap-abc");
        let t: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(t["notify"]["ntfy"]["topic"].as_str(), Some("tap-abc"));
        assert_eq!(t["stats"]["enabled"].as_bool(), Some(true));
    }
}
