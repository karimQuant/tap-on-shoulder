//! Presence probe for away-aware taps (plan v0.2 part B).
//!
//! `Away` = screen locked OR input idle ≥ `presence.idle_away`. Anything we
//! cannot read with certainty is `Unknown`, and `Unknown` keeps v0.1
//! behaviour (the normal grace period).
//!
//! - GNOME: `/usr/bin/gdbus` — Mutter `IdleMonitor.GetIdletime` (ms) and
//!   `ScreenSaver.GetActive` (bool).
//! - macOS: `/usr/sbin/ioreg -c IOHIDSystem -d 4 -k HIDIdleTime` (ns). Lock
//!   detection is unverified there, so it is idle only.
//!
//! Every probe command: absolute path, cleared environment (plus `LC_ALL=C`
//! and, for gdbus, `DBUS_SESSION_BUS_ADDRESS`), stdin and stderr null, own
//! process group, stdout capped at 4 KiB, killed and reaped after 500 ms.
//! Parsing accepts exactly one integer or boolean in the expected shape.
//! A reading is cached for 5 s in-process.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{self, Config, PresenceMode};
use crate::log::Entry;

pub const GDBUS: &str = "/usr/bin/gdbus";
pub const IOREG: &str = "/usr/sbin/ioreg";
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const STDOUT_CAP: usize = 4 << 10;
const CACHE_FOR: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    Away,
    Unknown,
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Present => "present",
            Presence::Away => "away",
            Presence::Unknown => "unknown",
        }
    }
}

/// Raw probe result, before `idle_away` is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Raw {
    pub idle_ms: Option<u64>,
    pub locked: Option<bool>,
    /// A probe command timed out (and was killed).
    pub timed_out: bool,
    /// Debug-only override (`TAP_TEST_PRESENCE*`).
    pub forced: Option<Presence>,
}

impl Raw {
    pub fn presence(&self, idle_away_s: u64) -> Presence {
        if let Some(p) = self.forced {
            return p;
        }
        if self.locked == Some(true) {
            return Presence::Away;
        }
        match self.idle_ms {
            Some(ms) if ms >= idle_away_s.saturating_mul(1000) => Presence::Away,
            Some(_) => Presence::Present,
            None => Presence::Unknown,
        }
    }
}

/// The probe this platform uses: `gnome`, `ioreg`, `test`, or `none`.
pub fn source() -> &'static str {
    if test_override().is_some() {
        return "test";
    }
    if cfg!(target_os = "macos") {
        if Path::new(IOREG).is_file() {
            return "ioreg";
        }
    } else if cfg!(target_os = "linux") && Path::new(GDBUS).is_file() {
        return "gnome";
    }
    "none"
}

/// Current presence for the sweeper: `Unknown` when `presence.mode = "off"`
/// (no probe runs), else a reading cached for 5 s.
pub fn current(cfg: &Config) -> Presence {
    if cfg.presence.mode == PresenceMode::Off {
        return Presence::Unknown;
    }
    cached().presence(cfg.presence.idle_away)
}

/// The `grace.away` an episode armed now gets: `None` when the hook runs in
/// an SSH session, because the local seat's lock and idle state say nothing
/// about a remote user (fail safe to v0.1).
pub fn grace_away(cfg: &Config) -> Option<i64> {
    (!remote_session()).then_some(cfg.grace.away as i64)
}

/// `SSH_CONNECTION`, `SSH_CLIENT` or `SSH_TTY` is set.
pub fn remote_session() -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

struct Cache {
    at: Instant,
    raw: Raw,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn cached() -> Raw {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = guard.as_ref() {
        if c.at.elapsed() < CACHE_FOR {
            return c.raw;
        }
    }
    let prev = guard.as_ref().map(|c| c.raw.timed_out);
    let raw = probe();
    if raw.timed_out && prev != Some(true) {
        Entry::new("sweep").kind("presence_timeout").write();
    }
    *guard = Some(Cache {
        at: Instant::now(),
        raw,
    });
    raw
}

/// Run the platform probe now (uncached).
pub fn probe() -> Raw {
    if let Some(r) = test_override() {
        return r;
    }
    match source() {
        "gnome" => probe_gnome(),
        "ioreg" => probe_ioreg(),
        _ => Raw::default(),
    }
}

/// Debug builds with `TAP_DEV=1` only:
/// - `TAP_TEST_PRESENCE_SLEEP_MS=<ms>` runs `/bin/sleep` through the probe
///   runner instead, so the timeout path yields `Unknown`.
/// - `TAP_TEST_PRESENCE_FILE=<path>`: the file holds `away`, `present` or
///   `unknown` and is re-read on every probe (switchable mid-wait).
/// - `TAP_TEST_PRESENCE=away|present|unknown`.
fn test_override() -> Option<Raw> {
    if !config::test_knobs() {
        return None;
    }
    if let Some(ms) = std::env::var("TAP_TEST_PRESENCE_SLEEP_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        let secs = format!("{:.3}", ms as f64 / 1000.0);
        let out = run_capped(Command::new("/bin/sleep").arg(secs), Instant::now());
        return Some(Raw {
            timed_out: out.is_err(),
            forced: Some(Presence::Unknown),
            ..Raw::default()
        });
    }
    let word = match std::env::var_os("TAP_TEST_PRESENCE_FILE") {
        Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
        None => std::env::var("TAP_TEST_PRESENCE").ok()?,
    };
    let p = match word.trim() {
        "away" => Presence::Away,
        "present" => Presence::Present,
        _ => Presence::Unknown,
    };
    Some(Raw {
        forced: Some(p),
        ..Raw::default()
    })
}

// ---------------------------------------------------------------- GNOME

fn probe_gnome() -> Raw {
    let Some(addr) = bus_address() else {
        return Raw::default();
    };
    let start = Instant::now();
    // Both calls share one 500 ms deadline.
    let idle = spawn(&mut gdbus(
        &addr,
        "org.gnome.Mutter.IdleMonitor",
        "/org/gnome/Mutter/IdleMonitor/Core",
        "org.gnome.Mutter.IdleMonitor.GetIdletime",
    ));
    let lock = spawn(&mut gdbus(
        &addr,
        "org.gnome.ScreenSaver",
        "/org/gnome/ScreenSaver",
        "org.gnome.ScreenSaver.GetActive",
    ));
    let idle = idle.map(|c| collect(c, start));
    let lock = lock.map(|c| collect(c, start));
    let timed_out = matches!(idle, Some(Err(Timeout))) || matches!(lock, Some(Err(Timeout)));
    Raw {
        idle_ms: idle
            .and_then(Result::ok)
            .flatten()
            .and_then(|s| parse_gdbus_uint64(&s)),
        locked: lock
            .and_then(Result::ok)
            .flatten()
            .and_then(|s| parse_gdbus_bool(&s)),
        timed_out,
        forced: None,
    }
}

fn gdbus(addr: &str, dest: &str, path: &str, method: &str) -> Command {
    let mut c = Command::new(GDBUS);
    c.args([
        "call",
        "--session",
        "--dest",
        dest,
        "--object-path",
        path,
        "--method",
        method,
    ])
    .env_clear()
    .env("LC_ALL", "C")
    .env("DBUS_SESSION_BUS_ADDRESS", addr);
    c
}

/// `DBUS_SESSION_BUS_ADDRESS` if set (sane length, no control characters),
/// else `unix:path=$XDG_RUNTIME_DIR/bus` only if that path is a socket owned
/// by our uid.
pub fn bus_address() -> Option<String> {
    if let Ok(a) = std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        if !a.is_empty() && a.len() <= 1024 && !a.chars().any(|c| c.is_control()) {
            return Some(a);
        }
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let dir = Path::new(&dir);
    if !dir.is_absolute() {
        return None;
    }
    let sock = dir.join("bus");
    let s = sock.to_str()?;
    if s.chars().any(|c| c.is_control() || c == ',' || c == ';') {
        return None;
    }
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let md = std::fs::symlink_metadata(&sock).ok()?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    (md.file_type().is_socket() && md.uid() == uid).then(|| format!("unix:path={s}"))
}

/// `(uint64 4658188851,)` → 4658188851. Exactly that shape, nothing else.
pub fn parse_gdbus_uint64(out: &str) -> Option<u64> {
    let inner = one_line(out)?
        .strip_prefix("(uint64 ")?
        .strip_suffix(",)")?;
    digits(inner)
}

/// `(true,)` / `(false,)`.
pub fn parse_gdbus_bool(out: &str) -> Option<bool> {
    match one_line(out)? {
        "(true,)" => Some(true),
        "(false,)" => Some(false),
        _ => None,
    }
}

/// The text with one optional trailing newline, which must be a single line.
fn one_line(out: &str) -> Option<&str> {
    let s = out.strip_suffix('\n').unwrap_or(out);
    (!s.is_empty() && !s.contains(['\n', '\r'])).then_some(s)
}

fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 20 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

// ---------------------------------------------------------------- macOS

fn probe_ioreg() -> Raw {
    let mut c = Command::new(IOREG);
    c.args(["-c", "IOHIDSystem", "-d", "4", "-k", "HIDIdleTime"])
        .env_clear()
        .env("LC_ALL", "C");
    let out = run_capped(&mut c, Instant::now());
    Raw {
        idle_ms: out
            .as_ref()
            .ok()
            .and_then(|o| o.as_deref())
            .and_then(parse_ioreg_idle_ns)
            .map(|ns| ns / 1_000_000),
        locked: None,
        timed_out: matches!(out, Err(Timeout)),
        forced: None,
    }
}

/// Exactly one line of the form `<tree prefix>"HIDIdleTime" = <digits>`,
/// where the prefix is only spaces and `|`. Other lines must not mention
/// `HIDIdleTime`. Returns nanoseconds.
pub fn parse_ioreg_idle_ns(out: &str) -> Option<u64> {
    let mut found = None;
    for line in out.lines() {
        if !line.contains("HIDIdleTime") {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let rest = line.trim_start_matches([' ', '|']);
        let v = rest.strip_prefix("\"HIDIdleTime\" = ")?;
        found = Some(digits(v.trim_end())?);
    }
    found
}

// ---------------------------------------------------------------- runner

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timeout;

/// Spawn with stdin/stderr null, stdout piped (non-blocking), own group.
fn spawn(c: &mut Command) -> Option<Child> {
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let child = c.spawn().ok()?;
    if let Some(out) = child.stdout.as_ref() {
        use std::os::fd::AsRawFd;
        let fd = out.as_raw_fd();
        // SAFETY: fcntl on a pipe fd we own.
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
    Some(child)
}

/// Wait until `start + 500 ms`, draining stdout (≤ 4 KiB). Returns the
/// output on a successful exit, `Ok(None)` on failure or oversize output,
/// and `Err(Timeout)` after killing and reaping a child that overran.
fn collect(mut child: Child, start: Instant) -> Result<Option<String>, Timeout> {
    let deadline = start + PROBE_TIMEOUT;
    let mut buf = Vec::new();
    let mut stdout = child.stdout.take();
    let mut oversize = false;
    loop {
        oversize |= drain(&mut stdout, &mut buf);
        match child.try_wait() {
            Ok(Some(status)) => {
                oversize |= drain(&mut stdout, &mut buf);
                if oversize || !status.success() {
                    return Ok(None);
                }
                return Ok(String::from_utf8(buf).ok());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                kill_group(&mut child);
                return Err(Timeout);
            }
        }
    }
}

/// Read what is available without blocking. Returns true (and stops
/// reading) once the output would exceed the cap.
fn drain(stdout: &mut Option<std::process::ChildStdout>, buf: &mut Vec<u8>) -> bool {
    let Some(s) = stdout.as_mut() else {
        return false;
    };
    let mut chunk = [0u8; 1024];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => {
                *stdout = None;
                return false;
            }
            Ok(n) => {
                if buf.len() + n > STDOUT_CAP {
                    *stdout = None;
                    return true;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false, // WouldBlock
        }
    }
}

fn run_capped(c: &mut Command, start: Instant) -> Result<Option<String>, Timeout> {
    match spawn(c) {
        Some(child) => collect(child, start),
        None => Ok(None),
    }
}

fn kill_group(child: &mut Child) {
    let pid = child.id() as i32;
    // SAFETY: the child leads its own process group (process_group(0)).
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_IDLE_AWAY_S;

    #[test]
    fn gdbus_uint64() {
        assert_eq!(
            parse_gdbus_uint64("(uint64 4658188851,)\n"),
            Some(4658188851)
        );
        assert_eq!(parse_gdbus_uint64("(uint64 0,)"), Some(0));
        for bad in [
            "",
            "\n",
            "(uint64 ,)\n",
            "(uint64 -1,)\n",
            "(uint64 12 ,)\n",
            "(uint64 1e9,)\n",
            "(uint32 5,)\n",
            "(uint64 5,)\n(uint64 6,)\n",
            "(uint64 5,)\n\n",
            "(uint64 99999999999999999999999,)\n",
            "(uint64 18446744073709551616,)\n",
            "(uint64 5,) trailing\n",
            " (uint64 5,)\n",
            "(uint64 ٥,)\n",
            "Error: GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown\n",
            "(true,)\n",
        ] {
            assert_eq!(parse_gdbus_uint64(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn gdbus_bool() {
        assert_eq!(parse_gdbus_bool("(true,)\n"), Some(true));
        assert_eq!(parse_gdbus_bool("(false,)\n"), Some(false));
        for bad in [
            "",
            "(True,)\n",
            "(true,)\n(false,)\n",
            "(true)\n",
            "true\n",
            "(true,) \n",
            "(uint64 1,)\n",
            "\u{0}(true,)",
        ] {
            assert_eq!(parse_gdbus_bool(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn ioreg_idle() {
        let ok = "+-o Root  <class IORegistryEntry, id 0x100000100, retain 25>\n  +-o IOHIDSystem  <class IOHIDSystem, id 0x100000abc>\n    {\n      \"HIDIdleTime\" = 1234567890\n    }\n";
        assert_eq!(parse_ioreg_idle_ns(ok), Some(1234567890));
        let piped = "    | |   \"HIDIdleTime\" = 42\n";
        assert_eq!(parse_ioreg_idle_ns(piped), Some(42));
        for bad in [
            "",
            "+-o IOHIDSystem\n",
            "  \"HIDIdleTime\" = 1\n  \"HIDIdleTime\" = 2\n",
            "  \"HIDIdleTime\" = -1\n",
            "  \"HIDIdleTime\" = 0x10\n",
            "  \"HIDIdleTime\" = 1 2\n",
            "  \"HIDIdleTime\" = <data>\n",
            "  x \"HIDIdleTime\" = 1\n",
            "  \"HIDIdleTimeX\" = 1\n",
            "  \"HIDIdleTime\" = 999999999999999999999\n",
        ] {
            assert_eq!(parse_ioreg_idle_ns(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn raw_to_presence() {
        let r = |idle_ms, locked| Raw {
            idle_ms,
            locked,
            ..Raw::default()
        };
        assert_eq!(r(None, None).presence(300), Presence::Unknown);
        assert_eq!(r(None, Some(false)).presence(300), Presence::Unknown);
        assert_eq!(r(None, Some(true)).presence(300), Presence::Away);
        assert_eq!(r(Some(1000), Some(true)).presence(300), Presence::Away);
        assert_eq!(
            r(Some(299_999), Some(false)).presence(300),
            Presence::Present
        );
        assert_eq!(r(Some(300_000), None).presence(300), Presence::Away);
        assert_eq!(
            r(Some(u64::MAX - 1), None).presence(u64::MAX),
            Presence::Present
        );
    }

    /// Spikes S11 (GNOME) and S12 (macOS): the live probe on this machine.
    /// `cargo test --lib presence::tests::live_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_probe() {
        let start = Instant::now();
        let raw = probe();
        let took = start.elapsed();
        eprintln!(
            "source {} raw {raw:?} presence {} in {took:?}",
            source(),
            raw.presence(DEFAULT_IDLE_AWAY_S).as_str()
        );
        assert!(!raw.timed_out);
        assert!(raw.idle_ms.is_some(), "no idle reading");
    }

    #[test]
    fn runner_kills_on_timeout_and_caps_output() {
        let start = Instant::now();
        let mut c = Command::new("/bin/sleep");
        c.arg("5");
        assert_eq!(run_capped(&mut c, start), Err(Timeout));
        assert!(start.elapsed() < Duration::from_secs(2));

        let mut c = Command::new("/bin/sh");
        c.args(["-c", "head -c 5000 /dev/zero"]);
        assert_eq!(run_capped(&mut c, Instant::now()), Ok(None), "oversize");

        let mut c = Command::new("/bin/sh");
        c.args(["-c", "echo '(true,)'"]);
        assert_eq!(
            run_capped(&mut c, Instant::now()),
            Ok(Some("(true,)\n".into()))
        );

        let mut c = Command::new("/bin/sh");
        c.args(["-c", "echo '(true,)'; exit 3"]);
        assert_eq!(run_capped(&mut c, Instant::now()), Ok(None), "failure");

        let mut c = Command::new("/nonexistent/gdbus");
        assert_eq!(run_capped(&mut c, Instant::now()), Ok(None));
    }
}
