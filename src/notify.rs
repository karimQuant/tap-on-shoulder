//! ntfy notifier (plan D8).
//!
//! JSON publish: `POST <server>/` with `{topic,title,message,priority,tags}`.
//! No header ever carries user text. ureq 3 with rustls, redirects disabled,
//! 10 s global timeout. `http://` only for loopback, RFC 1918, or `.local`.

use std::net::IpAddr;
use std::time::Duration;

use crate::config::{Ntfy, Privacy};
use crate::contract::{Category, Episode, Notifier, SendError, Tap};

const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a notifier could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyConfigError {
    NoTopic,
    BadServer,
    InsecureHttp,
}

impl NotifyConfigError {
    pub fn kind(self) -> &'static str {
        match self {
            NotifyConfigError::NoTopic => "no_topic",
            NotifyConfigError::BadServer => "bad_server",
            NotifyConfigError::InsecureHttp => "insecure_http",
        }
    }
}

pub struct NtfyNotifier {
    url: String,
    topic: String,
    token: Option<String>,
    agent: ureq::Agent,
}

/// Host part of an `http(s)://host[:port][/path]` URL.
fn host_of(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then_some((scheme, host))
}

/// Whether plain `http://` is acceptable for this host.
pub fn http_allowed(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private(),
        Ok(IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

/// Validate a server URL; returns the URL to POST to.
pub fn check_server(server: &str) -> Result<String, NotifyConfigError> {
    let (scheme, host) = host_of(server).ok_or(NotifyConfigError::BadServer)?;
    match scheme {
        "https" => {}
        "http" if http_allowed(host) => {}
        "http" => return Err(NotifyConfigError::InsecureHttp),
        _ => return Err(NotifyConfigError::BadServer),
    }
    Ok(format!("{}/", server.trim_end_matches('/')))
}

pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .build()
        .into()
}

impl NtfyNotifier {
    pub fn new(cfg: &Ntfy) -> Result<Self, NotifyConfigError> {
        let topic = cfg.topic.clone().ok_or(NotifyConfigError::NoTopic)?;
        let url = check_server(&cfg.server)?;
        let token = cfg
            .token_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_graphic()));
        Ok(NtfyNotifier {
            url,
            topic,
            token,
            agent: agent(SEND_TIMEOUT),
        })
    }
}

fn map_error(e: ureq::Error) -> SendError {
    use std::io::ErrorKind as K;
    use ureq::Error as E;
    match e {
        E::StatusCode(c) => SendError::Status(c),
        E::HostNotFound | E::ConnectionFailed | E::BadUri(_) | E::Tls(_) | E::Rustls(_) => {
            SendError::Connect("connect".into())
        }
        E::Timeout(t) => match t {
            ureq::Timeout::Resolve | ureq::Timeout::Connect => SendError::Connect("timeout".into()),
            _ => SendError::Uncertain("timeout".into()),
        },
        E::Io(io) => match io.kind() {
            K::ConnectionRefused
            | K::AddrNotAvailable
            | K::NetworkUnreachable
            | K::HostUnreachable
            | K::NotFound => SendError::Connect("io".into()),
            _ => SendError::Uncertain("io".into()),
        },
        _ => SendError::Uncertain("other".into()),
    }
}

/// Error kind for logs.
pub fn error_kind(e: &SendError) -> &'static str {
    match e {
        SendError::Connect(_) => "send_connect",
        SendError::Uncertain(_) => "send_uncertain",
        SendError::Status(_) => "send_status",
    }
}

impl Notifier for NtfyNotifier {
    fn send(&self, tap: &Tap) -> Result<(), SendError> {
        let body = serde_json::json!({
            "topic": self.topic,
            "title": tap.title,
            "message": tap.message,
            "priority": tap.priority,
            "tags": tap.tags,
        })
        .to_string();
        let mut req = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req.send(body.as_str()).map_err(map_error)?;
        let code = resp.status().as_u16();
        if (200..300).contains(&code) {
            Ok(())
        } else {
            Err(SendError::Status(code))
        }
    }
}

/// ntfy priority by category.
pub fn priority(c: Category) -> u8 {
    match c {
        Category::NeedsDecision | Category::NeedsPermission => 4,
        Category::Blocked => 3,
        Category::Done | Category::Info => 2,
    }
}

/// `45s`, `5m`, `2h10m`.
pub fn wait_text(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

/// Build the tap for an episode. `project` and `summary` are already clean.
pub fn tap_for(project: &str, ep: &Episode, now: i64, privacy: Privacy) -> Tap {
    let label = ep.category.label();
    let message = match privacy {
        Privacy::CategoryOnly => label.to_string(),
        Privacy::Summary if ep.summary.is_empty() => label.to_string(),
        Privacy::Summary => format!("{label}: {}", ep.summary),
    };
    Tap {
        title: format!("{project} · waiting {}", wait_text(now - ep.opened_at)),
        message,
        priority: priority(ep.category),
        tags: vec![ep.category.as_str().to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_policy() {
        assert!(check_server("https://ntfy.sh").is_ok());
        assert_eq!(
            check_server("https://ntfy.sh/").unwrap(),
            "https://ntfy.sh/"
        );
        assert!(check_server("http://127.0.0.1:8080").is_ok());
        assert!(check_server("http://localhost").is_ok());
        assert!(check_server("http://[::1]:80").is_ok());
        assert!(check_server("http://10.1.2.3").is_ok());
        assert!(check_server("http://172.16.0.1").is_ok());
        assert!(check_server("http://192.168.1.10").is_ok());
        assert!(check_server("http://nas.local").is_ok());
        assert_eq!(
            check_server("http://ntfy.sh"),
            Err(NotifyConfigError::InsecureHttp)
        );
        assert_eq!(
            check_server("http://172.32.0.1"),
            Err(NotifyConfigError::InsecureHttp)
        );
        assert_eq!(
            check_server("http://user@127.0.0.1"),
            Err(NotifyConfigError::BadServer)
        );
        assert_eq!(check_server("ftp://x"), Err(NotifyConfigError::BadServer));
    }

    #[test]
    fn wait_texts() {
        assert_eq!(wait_text(5), "5s");
        assert_eq!(wait_text(300), "5m");
        assert_eq!(wait_text(7800), "2h10m");
    }
}
