//! #72 lesson/guideline pipeline DTOs. Boundary ruling: the caller (office
//! rollout tap / harness) owns failure-trajectory mining and lesson text —
//! fusion-memory owns lesson storage, guideline merge, the regression-tax
//! gate and the injection surface. Zero model calls (Rule 5): every input
//! arrives caller-distilled.

use serde::{Deserialize, Serialize};

/// One mined lesson from a failure trajectory (caller-supplied, 1-3 per episode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistillEpisode {
    /// Tap trajectory reference (stored in sources for provenance; may be empty).
    #[serde(default)]
    pub episode_id: String,
    /// Session the failure happened in.
    pub session_id: String,
    /// Failure-mode cluster key (pagination / entity-resolution / no-evidence ...).
    pub failure_mode: String,
    /// Distilled lesson text (procedural rule the agent should follow next time).
    pub lesson: String,
}

/// Guideline projection for the injection surface (list_guidelines RPC).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuidelineEntry {
    pub id: String,
    pub failure_mode: String,
    /// trial | live | rejected.
    pub status: String,
    pub content: String,
    /// Lessons absorbed into this guideline.
    pub merge_count: u64,
    /// Lesson ids the guideline was merged from.
    pub sources: Vec<String>,
    pub created_timestamp: u64,
}

/// Regression-tax gate verdict for one guideline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuidelineVerdict {
    pub id: String,
    /// live | rejected.
    pub status: String,
    /// (success_with − success_without) − regressions, caller-measured.
    pub net_gain: f64,
}
