//! `tap sweep`: the single on-demand sweeper (plan D1, R2, R4, R5).
//!
//! - Holds a non-blocking `flock` on `state/sweeper.lock`; exits if taken.
//! - Loop: scan every session; sleep `min(earliest_due - now, 5 s)`.
//! - Exit protocol: when nothing is open, release the lock, re-scan once, and
//!   re-acquire if anything is open again (closes the exit race).
//! - Delivery (at most once): `open → sending` under the session lock, send
//!   without the lock, then `notified | open(retry) | failed | sent_uncertain`
//!   only if the episode is still the same `sending` one (write guard).

use std::collections::HashSet;
use std::time::Duration;

use crate::config;
use crate::contract::{Category, Episode, EpisodeStatus, Notifier, SendError, SessionState};
use crate::log::Entry;
use crate::notify::{self, NtfyNotifier};
use crate::state::{self, FileLock, Loaded, Store};

const MAX_SLEEP_MS: i64 = 5_000;
const MIN_SLEEP_MS: i64 = 20;
const SENDING_STALE_S: i64 = 120;
const MAX_ATTEMPTS: u32 = 3;

/// Timing knobs; shortened by `TAP_DEV=1 TAP_DEV_FAST=1` for tests.
#[derive(Debug, Clone, Copy)]
struct Timing {
    retry_base_s: i64,
    postpone_step_s: i64,
    postpone_cap_s: i64,
    sample_gap: Duration,
}

impl Timing {
    fn current() -> Timing {
        if config::dev_fast() {
            Timing {
                retry_base_s: 1,
                postpone_step_s: 1,
                postpone_cap_s: 3,
                sample_gap: Duration::from_millis(300),
            }
        } else {
            Timing {
                retry_base_s: 15,
                postpone_step_s: 30,
                postpone_cap_s: 600,
                sample_gap: Duration::from_secs(2),
            }
        }
    }
}

struct Sweeper {
    store: Store,
    timing: Timing,
    corrupt_logged: HashSet<String>,
}

/// Run the sweeper until no episode is open.
pub fn run() {
    let Ok(store) = Store::open(false) else {
        return;
    };
    let lock_path = store.dir().join("sweeper.lock");
    let Ok(Some(lock)) = FileLock::try_acquire(&lock_path) else {
        return;
    };
    let mut lock = Some(lock);
    let mut sw = Sweeper {
        store,
        timing: Timing::current(),
        corrupt_logged: HashSet::new(),
    };
    Entry::new("sweep").transition("started").write();
    loop {
        if !sw.store.dir().is_dir() {
            break;
        }
        match sw.pass() {
            Some(wake_ms) => {
                let ms = (wake_ms - state::now_ms()).clamp(MIN_SLEEP_MS, MAX_SLEEP_MS);
                std::thread::sleep(Duration::from_millis(ms as u64));
            }
            None => {
                drop(lock.take());
                if !sw.has_work() {
                    break;
                }
                match FileLock::try_acquire(&lock_path) {
                    Ok(Some(l)) => lock = Some(l),
                    // Another sweeper took over.
                    _ => return,
                }
            }
        }
    }
    drop(lock);
    Entry::new("sweep").transition("exited").write();
}

fn short(e: Entry, sid: &str, ep: &Episode) -> Entry {
    e.session(sid).episode(&ep.id).category(ep.category)
}

impl Sweeper {
    /// One scan over all sessions. Returns the earliest wake time (ms), or
    /// `None` when nothing is pending.
    fn pass(&mut self) -> Option<i64> {
        let mut wake: Option<i64> = None;
        for sid in self.store.sessions() {
            if let Some(t) = self.session(&sid) {
                wake = Some(wake.map_or(t, |w| w.min(t)));
            }
        }
        wake
    }

    /// Read-only: is any episode open or in flight?
    fn has_work(&self) -> bool {
        self.store.sessions().iter().any(|sid| {
            matches!(self.store.load(sid), Loaded::Ok(s) if s.episode.as_ref().is_some_and(|e|
                matches!(e.status, EpisodeStatus::Open | EpisodeStatus::Sending)))
        })
    }

    fn snapshot(&mut self, sid: &str) -> Option<SessionState> {
        let _l = self.store.lock(sid).ok()?;
        match self.store.load(sid) {
            Loaded::Ok(s) => Some(*s),
            Loaded::Missing => None,
            Loaded::Corrupt => {
                if self.corrupt_logged.insert(sid.to_string()) {
                    Entry::new("sweep")
                        .session(sid)
                        .kind("corrupt_state")
                        .write();
                }
                None
            }
        }
    }

    /// Apply `f` to the episode under the session lock, only if it is still
    /// episode `id` with `status` and `attempts`. Returns the updated episode.
    fn guarded(
        &self,
        sid: &str,
        id: &str,
        status: EpisodeStatus,
        attempts: u32,
        f: impl FnOnce(&mut Episode),
    ) -> Option<Episode> {
        let _l = self.store.lock(sid).ok()?;
        let Loaded::Ok(mut st) = self.store.load(sid) else {
            return None;
        };
        let ep = st.episode.as_mut()?;
        if ep.id != id || ep.status != status || ep.attempts != attempts {
            return None;
        }
        f(ep);
        let out = ep.clone();
        st.updated_at = state::now();
        self.store.save(&st).ok()?;
        Some(out)
    }

    fn close(&self, sid: &str, ep: &Episode, status: EpisodeStatus, reason: &'static str) {
        let done = self.guarded(sid, &ep.id, ep.status, ep.attempts, |e| {
            e.status = status;
            e.closed_reason = Some(reason.into());
        });
        if done.is_some() {
            short(Entry::new("sweep"), sid, ep)
                .transition(status_name(status))
                .kind(reason)
                .write();
        }
    }

    fn session(&mut self, sid: &str) -> Option<i64> {
        let snap = self.snapshot(sid)?;
        let ep = snap.episode.clone()?;
        let now = state::now();
        match ep.status {
            EpisodeStatus::Sending => {
                let since = ep.sending_since.unwrap_or(ep.due_at);
                if now - since >= SENDING_STALE_S {
                    self.close(sid, &ep, EpisodeStatus::SentUncertain, "stale_sending");
                    return None;
                }
                return Some((since + SENDING_STALE_S) * 1000);
            }
            EpisodeStatus::Open => {}
            _ => return None,
        }
        let due = ep.due_at.max(ep.retry_at.unwrap_or(0));
        if now < due {
            return Some(due * 1000);
        }

        // D4/R6: session liveness.
        if let Some(c) = &snap.claude {
            if !crate::proc::same_process(c) {
                self.close(sid, &ep, EpisodeStatus::Closed, "session_gone");
                return None;
            }
        }

        // D3/R4: an approved tool may already be running.
        if ep.category == Category::NeedsPermission && ep.postponed_s < self.timing.postpone_cap_s {
            if let Some(c) = &snap.claude {
                let since = ep.bound_at.unwrap_or(ep.opened_at);
                if crate::proc::tool_running(c.pid, since, self.timing.sample_gap) {
                    let step = self.timing.postpone_step_s;
                    let now = state::now();
                    let upd = self.guarded(sid, &ep.id, EpisodeStatus::Open, ep.attempts, |e| {
                        e.postponed_s += step;
                        e.due_at = now + step;
                        e.retry_at = None;
                    });
                    return match upd {
                        Some(e) => {
                            short(Entry::new("sweep"), sid, &e)
                                .transition("postponed")
                                .kind("tool_running")
                                .write();
                            Some(e.due_at * 1000)
                        }
                        // Changed under us; look again soon.
                        None => Some(state::now_ms()),
                    };
                }
            }
        }

        self.deliver(sid, &snap.project, &ep)
    }

    /// Claim, send, and record the result.
    fn deliver(&self, sid: &str, project: &str, ep: &Episode) -> Option<i64> {
        let cfg = config::load(None).config;
        let notifier = match NtfyNotifier::new(&cfg.ntfy) {
            Ok(n) => n,
            Err(e) => {
                let kind = e.kind();
                if self
                    .guarded(sid, &ep.id, EpisodeStatus::Open, ep.attempts, |x| {
                        x.status = EpisodeStatus::Failed;
                        x.closed_reason = Some(kind.into());
                    })
                    .is_some()
                {
                    short(Entry::new("sweep"), sid, ep)
                        .transition("failed")
                        .kind(kind)
                        .write();
                }
                return None;
            }
        };
        let now = state::now();
        let claimed = self.guarded(sid, &ep.id, EpisodeStatus::Open, ep.attempts, |e| {
            e.status = EpisodeStatus::Sending;
            e.sending_since = Some(now);
        })?;
        short(Entry::new("sweep"), sid, &claimed)
            .transition("sending")
            .write();

        let tap = notify::tap_for(project, &claimed, now, cfg.ntfy.privacy);
        let result = notifier.send(&tap);

        let base = self.timing.retry_base_s;
        let now = state::now();
        let kind = result.as_ref().err().map(notify::error_kind);
        let updated = self.guarded(
            sid,
            &claimed.id,
            EpisodeStatus::Sending,
            claimed.attempts,
            |e| {
                e.sending_since = None;
                match &result {
                    Ok(()) => e.status = EpisodeStatus::Notified,
                    Err(SendError::Connect(_)) => {
                        e.attempts += 1;
                        if e.attempts >= MAX_ATTEMPTS {
                            e.status = EpisodeStatus::Failed;
                            e.closed_reason = Some("unreachable".into());
                        } else {
                            e.status = EpisodeStatus::Open;
                            e.retry_at = Some(now + base * (1 << (e.attempts - 1)));
                        }
                    }
                    Err(SendError::Status(_)) => {
                        e.attempts += 1;
                        e.status = EpisodeStatus::Failed;
                        e.closed_reason = Some("http_status".into());
                    }
                    Err(SendError::Uncertain(_)) => {
                        e.attempts += 1;
                        e.status = EpisodeStatus::SentUncertain;
                    }
                }
            },
        );
        let Some(e) = updated else {
            // A hook closed it mid-send (hook wins) or the state is gone.
            short(Entry::new("sweep"), sid, &claimed)
                .kind("guard_lost")
                .write();
            return None;
        };
        let mut entry = short(Entry::new("sweep"), sid, &e).transition(status_name(e.status));
        if let Some(k) = kind {
            entry = entry.kind(k);
        }
        entry.write();
        (e.status == EpisodeStatus::Open).then(|| e.retry_at.unwrap_or(now) * 1000)
    }
}

fn status_name(s: EpisodeStatus) -> &'static str {
    match s {
        EpisodeStatus::Open => "open",
        EpisodeStatus::Sending => "sending",
        EpisodeStatus::Notified => "notified",
        EpisodeStatus::SentUncertain => "sent_uncertain",
        EpisodeStatus::Failed => "failed",
        EpisodeStatus::Closed => "closed",
    }
}
