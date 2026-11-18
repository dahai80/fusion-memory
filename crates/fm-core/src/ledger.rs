//! #68 B4: memory net-value ledger types.
//!
//! Boundary ruling (issue #68): fusion-memory owns the ledger and the
//! auto-shutdown policy; fusion-harness owns cost measurement. fusion-memory
//! never estimates token cost itself — every record carries a caller-supplied
//! `tax` (and optional caller-reported `benefit`). Net value = benefit − tax.

use serde::{Deserialize, Serialize};

/// One recorded memory operation cost, supplied by the caller (harness funnel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpCostRecord {
    /// Operation kind (e.g. "commit", "retrieve", "consolidate").
    pub op: String,
    /// Operation class the shutdown policy gates on (e.g. "retrieve:playbook").
    pub class: String,
    /// Optional memory the op touched.
    #[serde(default)]
    pub memory_id: Option<String>,
    /// Caller-reported benefit (utility units, harness-defined).
    #[serde(default)]
    pub benefit: f64,
    /// Caller-measured tax (tokens + attention, harness-measured). Never estimated here.
    pub tax: f64,
    /// Epoch ms (0 = engine fills from clock).
    #[serde(default)]
    pub at: u64,
}

/// Aggregated net value for one operation class (report row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpClassNet {
    pub class: String,
    /// Ops counted in the aggregation window.
    pub ops: u64,
    pub benefit: f64,
    pub tax: f64,
    /// benefit − tax over the window.
    pub net: f64,
    /// Whether the class is currently auto-shut down.
    pub shutdown: bool,
}

/// Result of recording one op cost — includes the live shutdown decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerStatus {
    pub class: String,
    /// Ops of this class in the current window.
    pub ops: u64,
    /// Window-average net value for the class.
    pub net: f64,
    /// Whether the class is shut down after this record.
    pub shutdown: bool,
    /// True when this record itself triggered the shutdown transition.
    pub just_shutdown: bool,
}
