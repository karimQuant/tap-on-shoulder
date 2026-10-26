//! Process heuristics (plan D3/D4, R4, R6).
//!
//! - Claude detection: walk up to 4 ancestors; a process is Claude if its comm
//!   basename is `claude`, or `node` with an argv entry containing `claude`.
//!   Otherwise fall back to the parent of the hook's `sh`.
//! - Identity: Linux `/proc/<pid>/stat` field 22; macOS `ps -o stat=,lstart=`.
//!   A zombie counts as gone on both. Every `ps` runs with `LC_ALL=C TZ=UTC0`.
//! - Descendants: `/proc` on Linux, `ps -axo pid=,ppid=,etime=,comm=` elsewhere.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::contract::ProcId;

const MAX_ANCESTORS: usize = 4;

/// One row of a process listing.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    /// Start time, seconds since the epoch (fractional on Linux).
    pub start_epoch: f64,
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// The Claude process for this hook, if one can be identified.
///
/// In debug builds with `TAP_DEV=1`, `TAP_TEST_CLAUDE_PID=<pid>` injects the
/// pid, and `TAP_TEST_CLAUDE_PID=none` disables detection.
pub fn detect_claude() -> Option<ProcId> {
    if crate::config::test_knobs() {
        if let Ok(v) = std::env::var("TAP_TEST_CLAUDE_PID") {
            return v.parse::<i32>().ok().and_then(identity);
        }
    }
    // SAFETY: getppid has no preconditions.
    let parent = unsafe { libc::getppid() };
    let mut pid = parent;
    for _ in 0..MAX_ANCESTORS {
        if pid <= 1 {
            break;
        }
        let Some((comm, ppid)) = comm_and_ppid(pid) else {
            break;
        };
        let name = basename(&comm);
        if name == "claude" || (name == "node" && argv(pid).iter().any(|a| a.contains("claude"))) {
            return identity(pid);
        }
        pid = ppid;
    }
    let (comm, ppid) = comm_and_ppid(parent)?;
    if matches!(basename(&comm), "sh" | "bash" | "dash" | "zsh") && ppid > 1 {
        return identity(ppid);
    }
    None
}

/// Whether `id` still names the same live process.
pub fn same_process(id: &ProcId) -> bool {
    identity(id.pid).is_some_and(|now| now.start == id.start)
}

/// Process identity: pid plus a start-time token.
pub fn identity(pid: i32) -> Option<ProcId> {
    if pid <= 0 {
        return None;
    }
    start_token(pid).map(|start| ProcId { pid, start })
}

#[cfg(target_os = "linux")]
fn read_stat(pid: i32) -> Option<(String, Vec<String>)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    // Fields after the comm start at field 3 (state).
    let rest = s
        .get(close + 1..)?
        .split_whitespace()
        .map(String::from)
        .collect();
    Some((comm, rest))
}

#[cfg(target_os = "linux")]
fn start_token(pid: i32) -> Option<String> {
    // Field 22 is index 19 in `rest` (which starts at field 3). A zombie
    // (state Z/X) counts as gone.
    let (_, rest) = read_stat(pid)?;
    if matches!(rest.first().map(String::as_str), Some("Z" | "X")) {
        return None;
    }
    rest.get(19).cloned()
}

#[cfg(not(target_os = "linux"))]
fn start_token(pid: i32) -> Option<String> {
    // `stat` first: it is one word, while `lstart` contains spaces. A zombie
    // (state `Z...`) counts as gone.
    let out = ps(&["-o", "stat=,lstart=", "-p", &pid.to_string()])?;
    parse_stat_lstart(&out)
}

/// Parse one `ps -o stat=,lstart=` row into the start token, or `None` for a
/// zombie or an empty row.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_stat_lstart(out: &str) -> Option<String> {
    let (stat, lstart) = out.trim().split_once(char::is_whitespace)?;
    if stat.starts_with('Z') {
        return None;
    }
    let t = lstart.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(target_os = "linux")]
fn comm_and_ppid(pid: i32) -> Option<(String, i32)> {
    let (comm, rest) = read_stat(pid)?;
    Some((comm, rest.get(1)?.parse().ok()?))
}

#[cfg(not(target_os = "linux"))]
fn comm_and_ppid(pid: i32) -> Option<(String, i32)> {
    let out = ps(&["-o", "ppid=,comm=", "-p", &pid.to_string()])?;
    let line = out.trim();
    let (ppid, comm) = line.split_once(char::is_whitespace)?;
    Some((comm.trim().to_string(), ppid.trim().parse().ok()?))
}

#[cfg(target_os = "linux")]
fn argv(pid: i32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
fn argv(pid: i32) -> Vec<String> {
    ps(&["-o", "args=", "-p", &pid.to_string()])
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn ps(args: &[&str]) -> Option<String> {
    // Fixed locale and time zone: `lstart` text must not change between a
    // hook and the sweeper, whatever their environments.
    let out = Command::new("ps")
        .args(args)
        .env("LC_ALL", "C")
        .env("TZ", "UTC0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `ps` `etime` (`[[dd-]hh:]mm:ss`) into seconds.
pub fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(days * 86400 + secs)
}

/// All processes with parent and start time.
pub fn list() -> Vec<ProcInfo> {
    #[cfg(target_os = "linux")]
    {
        list_proc()
    }
    #[cfg(not(target_os = "linux"))]
    {
        list_ps()
    }
}

#[cfg(target_os = "linux")]
fn list_proc() -> Vec<ProcInfo> {
    // Boot time from uptime (10 ms resolution); `btime` is whole seconds.
    let uptime = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0);
    let btime = crate::state::now_ms() as f64 / 1000.0 - uptime;
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(|pid| {
            let (_, rest) = read_stat(pid)?;
            let ppid = rest.get(1)?.parse().ok()?;
            let ticks: f64 = rest.get(19)?.parse().ok()?;
            Some(ProcInfo {
                pid,
                ppid,
                start_epoch: btime + ticks / hz,
            })
        })
        .collect()
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn list_ps() -> Vec<ProcInfo> {
    let now = crate::state::now() as f64;
    let Some(out) = ps(&["-axo", "pid=,ppid=,etime=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let etime = parse_etime(it.next()?)?;
            Some(ProcInfo {
                pid,
                ppid,
                start_epoch: now - etime as f64,
            })
        })
        .collect()
}

/// Descendants of `root` whose whole ancestor chain up to `root` started at
/// or after `since` (plan R4). Returns `(pid, start_epoch)` pairs.
pub fn fresh_descendants(procs: &[ProcInfo], root: i32, since: i64) -> Vec<(i32, f64)> {
    let by_pid: HashMap<i32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let since = since as f64;
    let mut out = Vec::new();
    for p in procs {
        if p.pid == root {
            continue;
        }
        let mut cur = p;
        let mut fresh = true;
        let mut reached = false;
        for _ in 0..64 {
            if cur.start_epoch < since {
                fresh = false;
                break;
            }
            if cur.ppid == root {
                reached = true;
                break;
            }
            match by_pid.get(&cur.ppid) {
                Some(next) => cur = next,
                None => break,
            }
        }
        if reached && fresh {
            out.push((p.pid, p.start_epoch));
        }
    }
    out.sort_unstable_by_key(|p| p.0);
    out
}

/// Plan D3/R4: a descendant of `root` that started after `since` is alive in
/// two samples `gap` apart.
pub fn tool_running(root: i32, since: i64, gap: Duration) -> bool {
    let first = fresh_descendants(&list(), root, since);
    if first.is_empty() {
        return false;
    }
    std::thread::sleep(gap);
    let second = fresh_descendants(&list(), root, since);
    // Same pid with (nearly) the same start time: derived start times jitter
    // slightly between samples (and by up to 1 s with `ps etime`).
    first.iter().any(|(pid, t)| {
        second
            .iter()
            .any(|(p2, t2)| p2 == pid && (t - t2).abs() < 1.5)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parsing() {
        assert_eq!(parse_etime("00:05"), Some(5));
        assert_eq!(parse_etime("01:02:03"), Some(3723));
        assert_eq!(parse_etime("2-00:00:01"), Some(172801));
        assert_eq!(parse_etime("x"), None);
    }

    #[test]
    fn fresh_descendants_requires_whole_chain_after_since() {
        let p = |pid, ppid, start| ProcInfo {
            pid,
            ppid,
            start_epoch: start,
        };
        let procs = vec![
            p(10, 1, 0.0),    // claude
            p(11, 10, 50.0),  // old shell under claude
            p(12, 11, 150.0), // new child of old shell: chain not fresh
            p(13, 10, 120.0), // new tool
            p(14, 13, 130.0), // its child
            p(20, 1, 200.0),  // unrelated
        ];
        let pids: Vec<i32> = fresh_descendants(&procs, 10, 100)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(pids, vec![13, 14]);
    }

    #[test]
    fn stat_lstart_parsing() {
        assert_eq!(
            parse_stat_lstart("Ss   Mon Oct 26 09:00:01 2026\n").as_deref(),
            Some("Mon Oct 26 09:00:01 2026")
        );
        assert_eq!(parse_stat_lstart("Z+   Mon Oct 26 09:00:01 2026"), None);
        assert_eq!(parse_stat_lstart(""), None);
    }

    #[test]
    fn identity_of_self_is_stable() {
        let me = identity(std::process::id() as i32).expect("own identity");
        assert!(same_process(&me));
        assert!(!same_process(&ProcId {
            pid: me.pid,
            start: "not-a-start".into()
        }));
    }
}
