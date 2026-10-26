//! `tap setup [--force] [--server URL]`: write the user config (0600) with a
//! random topic and print the subscribe URL.

use std::os::unix::fs::DirBuilderExt;

use crate::config::{self, DEFAULT_SERVER};
use crate::notify;

const TOPIC_RANDOM_CHARS: usize = 24;
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// `tap-` followed by 24 random lowercase alphanumerics.
pub fn random_topic() -> String {
    let mut out = String::from("tap-");
    while out.len() < 4 + TOPIC_RANDOM_CHARS {
        let mut buf = [0u8; 32];
        if getrandom::fill(&mut buf).is_err() {
            break;
        }
        for b in buf {
            // Rejection sampling keeps the distribution uniform.
            if (b as usize) < 252 && out.len() < 4 + TOPIC_RANDOM_CHARS {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    out
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
         collect = false\n"
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
    let topic = random_topic();
    if crate::state::write_atomic(&path, config_text(&server, &topic).as_bytes()).is_err() {
        eprintln!("cannot write {}", path.display());
        return 1;
    }
    println!("wrote {}", path.display());
    println!("subscribe: {server}/{topic}");
    println!("Open that URL in the ntfy app (or browser), then run `tap test`.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_shape() {
        let t = random_topic();
        assert_eq!(t.len(), 28);
        assert!(t.starts_with("tap-"));
        assert!(t[4..].bytes().all(|b| ALPHABET.contains(&b)));
        assert!(config::valid_topic(&t));
        assert_ne!(t, random_topic());
    }

    #[test]
    fn generated_config_parses() {
        let text = config_text("https://ntfy.sh", "tap-abc");
        let t: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(t["notify"]["ntfy"]["topic"].as_str(), Some("tap-abc"));
    }
}
