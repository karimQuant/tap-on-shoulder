//! tap-on-shoulder: taps you when a Claude Code session has been waiting on you.
//!
//! Module ownership (team lanes) is documented in `.omc/plans/` and
//! `docs/design/contract.md`; shared types live in [`contract`].

pub mod classify;
pub mod cli;
pub mod config;
pub mod contract;
pub mod hook;
pub mod log;
pub mod notify;
pub mod presence;
pub mod proc;
pub mod sanitize;
pub mod state;
pub mod stats;
pub mod sweep;
