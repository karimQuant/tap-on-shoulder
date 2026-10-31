//! Shared types for every lane. See `docs/design/contract.md`.
//!
//! Changing anything here that affects classification requires bumping
//! `FEATURE_SPEC` and regenerating `assets/head.json` and the parity fixtures.

use serde::{Deserialize, Serialize};

/// Bump when tokenization, features, or head input layout change.
pub const FEATURE_SPEC: u32 = 1;
/// State file schema version.
pub const STATE_VERSION: u32 = 1;

pub const TEXT_MAX_CHARS: usize = 16_384;
pub const TAIL_CHARS: usize = 600;
pub const MAX_TOKEN_IDS: usize = 512;
pub const EMBED_DIM: usize = 256;
pub const N_FEATURES: usize = 12;
pub const SUMMARY_MAX_CHARS: usize = 80;
pub const STDIN_MAX_BYTES: usize = 8 << 20;
pub const MAX_PENDING_TOOLS: usize = 32;
pub const DEFAULT_LEAN_THRESHOLD: f32 = 0.25;

/// Episode category. `NeedsPermission` only ever comes from events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    NeedsDecision,
    NeedsPermission,
    Blocked,
    Done,
    Info,
}

impl Category {
    /// The four classes the model predicts, in head output order.
    pub const MODEL_CLASSES: [Category; 4] = [
        Category::NeedsDecision,
        Category::Blocked,
        Category::Done,
        Category::Info,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::NeedsDecision => "needs_decision",
            Category::NeedsPermission => "needs_permission",
            Category::Blocked => "blocked",
            Category::Done => "done",
            Category::Info => "info",
        }
    }

    /// Human label used in notifications.
    pub fn label(self) -> &'static str {
        match self {
            Category::NeedsDecision => "needs a decision",
            Category::NeedsPermission => "waiting on a permission prompt",
            Category::Blocked => "is blocked",
            Category::Done => "finished",
            Category::Info => "stopped",
        }
    }

    /// Re-arm priority: higher replaces lower (plan R3).
    pub fn priority(self) -> u8 {
        match self {
            Category::NeedsPermission => 5,
            Category::NeedsDecision => 4,
            Category::Blocked => 3,
            Category::Done => 2,
            Category::Info => 1,
        }
    }
}

/// Output of any classifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub category: Category,
    /// Already sanitized, ≤ `SUMMARY_MAX_CHARS`.
    pub summary: String,
    /// Probabilities over `Category::MODEL_CLASSES`, if a model ran.
    pub probs: Option<[f32; 4]>,
}

/// Classifies the final assistant message of a `Stop` event.
pub trait Classifier {
    fn classify(&self, text: &str) -> Classification;
    /// `embed`, `features`, or `rules`.
    fn variant(&self) -> &'static str;
}

/// `assets/head.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Head {
    pub version: u32,
    pub feature_spec: u32,
    /// `embed` (dim 524) or `features` (dim 12).
    pub variant: String,
    pub dim: usize,
    pub classes: Vec<String>,
    #[serde(rename = "W")]
    pub w: Vec<Vec<f32>>,
    pub b: Vec<f32>,
    pub lean_threshold: f32,
    pub model_rev: String,
    #[serde(default)]
    pub trained_on: serde_json::Value,
    /// Grouped-CV macro-F1 and tap recall recorded by `model/train.py`.
    #[serde(default)]
    pub cv_macro_f1: Option<f32>,
    #[serde(default)]
    pub cv_tap_recall: Option<f32>,
}

/// One entry of `tests/fixtures/parity.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityFixture {
    pub text: String,
    pub ids_tail: Vec<u32>,
    pub ids_text: Vec<u32>,
    pub features: Vec<f32>,
    pub probs: Vec<f32>,
    pub label: String,
}

// ---------------------------------------------------------------- hook input

/// Fields read from hook stdin. Everything else is ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HookInput {
    pub hook_event_name: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub background_tasks: Option<Vec<BackgroundTask>>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub notification_type: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BackgroundTask {
    #[serde(default)]
    pub status: String,
}

// ---------------------------------------------------------------- state

/// `${CLAUDE_PLUGIN_DATA}/state/<session_id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub session_id: String,
    /// Basename of `cwd`.
    pub project: String,
    pub claude: Option<ProcId>,
    #[serde(default)]
    pub pending_tools: Vec<PendingTool>,
    pub episode: Option<Episode>,
    pub updated_at: i64,
}

/// A process identity that survives PID reuse checks (plan R6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcId {
    pub pid: i32,
    /// Linux: `/proc/<pid>/stat` field 22. macOS: `ps -o lstart=` text.
    pub start: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTool {
    pub tool_use_id: String,
    pub tool_name: String,
    /// First 16 hex of sha256(canonical tool_input JSON).
    pub input_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub status: EpisodeStatus,
    pub category: Category,
    pub summary: String,
    /// Event kind that opened it, e.g. `Stop`, `PermissionRequest`.
    pub trigger: String,
    /// Tool binding for bound episodes (plan R3).
    pub tool_use_id: Option<String>,
    /// Ambiguous binding: every pending call of the requested tool. The
    /// episode closes once none of them is pending. `tool_use_id` is `None`.
    #[serde(default)]
    pub candidate_ids: Vec<String>,
    /// Binding by name only (no pending call of that tool was known): a
    /// `Post*` for this tool closes the episode.
    #[serde(default)]
    pub bound_tool_name: Option<String>,
    pub opened_at: i64,
    /// Last time this episode was bound to a tool (plan R4 process check).
    #[serde(default)]
    pub bound_at: Option<i64>,
    pub due_at: i64,
    #[serde(default)]
    pub confirmed_at: Option<i64>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub retry_at: Option<i64>,
    #[serde(default)]
    pub sending_since: Option<i64>,
    /// Total seconds the permission tap was postponed (plan R4).
    #[serde(default)]
    pub postponed_s: i64,
    #[serde(default)]
    pub closed_reason: Option<String>,
    /// When a tap was sent (`notified`) or probably sent (`sent_uncertain`).
    #[serde(default)]
    pub notified_at: Option<i64>,
    /// Set when a user action ends a `notified | sent_uncertain | failed`
    /// episode (the idle-ledger "finish" step): the ledger reason. A finished
    /// episode is never recorded again.
    #[serde(default)]
    pub finished_reason: Option<String>,
    /// Effective `grace.away` (s) when armed; project config may shorten it.
    /// `None` (armed over SSH, or state from v0.1): presence is ignored.
    #[serde(default)]
    pub grace_away_s: Option<i64>,
    /// The last claim happened before `due_at` because the user was away.
    #[serde(default)]
    pub sent_early: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeStatus {
    Open,
    Sending,
    Notified,
    SentUncertain,
    Failed,
    Closed,
}

impl EpisodeStatus {
    pub fn is_open(self) -> bool {
        matches!(self, EpisodeStatus::Open)
    }
}

// ---------------------------------------------------------------- notify

/// What a notifier sends. Every string is already sanitized.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tap {
    pub title: String,
    pub message: String,
    /// ntfy priority 1..=5.
    pub priority: u8,
    pub tags: Vec<String>,
}

#[derive(Debug)]
pub enum SendError {
    /// Nothing was written; safe to retry.
    Connect(String),
    /// The request may have been delivered; never retry (at-most-once).
    Uncertain(String),
    /// Server answered with an error status.
    Status(u16),
}

pub trait Notifier {
    fn send(&self, tap: &Tap) -> Result<(), SendError>;
}
