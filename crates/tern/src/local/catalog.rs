//! Recommended local models per tier, by this Mac's memory. Data, not
//! logic: tags change often, so edit this table (and check each tag with a
//! real tool-use run). Tags come from docs/research/local-models.md §7 and
//! are not yet verified by a tern eval.
//!
//! Tiers: **agent** runs tasks with tools (long loops, where small models
//! drift), **fast** writes summaries and role lines, **decision** makes
//! short choices (who answers, is this worth a push). Small models do the
//! last two well, so every Mac gets a useful pair even when the agent tier
//! is modest.

use serde::{Deserialize, Serialize};

const GIB: u64 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Agent,
    Fast,
    Decision,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Agent, Tier::Fast, Tier::Decision];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Agent => "agent",
            Tier::Fast => "fast",
            Tier::Decision => "decision",
        }
    }

    /// Needs tool calls (the fast tier only writes text).
    pub fn needs_tools(self) -> bool {
        !matches!(self, Tier::Fast)
    }
}

/// One recommendation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pick {
    pub tier: Tier,
    pub model: &'static str,
    /// Download size.
    pub size_gb: f32,
    /// One short line.
    pub blurb: &'static str,
    /// Honest caveat for this Mac, if any.
    pub note: Option<&'static str>,
}

struct Row {
    tier: Tier,
    /// Mac memory in GB at least.
    min_gb: u64,
    model: &'static str,
    size_gb: f32,
    blurb: &'static str,
    note: Option<&'static str>,
}

const fn row(
    tier: Tier,
    min_gb: u64,
    model: &'static str,
    size_gb: f32,
    blurb: &'static str,
    note: Option<&'static str>,
) -> Row {
    Row {
        tier,
        min_gb,
        model,
        size_gb,
        blurb,
        note,
    }
}

/// Only if a tier has no row, which the tests rule out.
const FALLBACK: Row = row(
    Tier::Agent,
    0,
    "granite4.2:3b",
    2.0,
    "Small and fast. Light tasks.",
    None,
);

const TABLE: &[Row] = &[
    row(
        Tier::Agent,
        0,
        "granite4.2:3b",
        2.0,
        "Small and fast. Light tasks.",
        Some("On this Mac, expect chat and light tasks, not long jobs."),
    ),
    row(
        Tier::Agent,
        12,
        "qwen3.5:9b",
        6.6,
        "Good all-round for everyday tasks.",
        None,
    ),
    row(
        Tier::Agent,
        24,
        "qwen3.6:27b",
        17.0,
        "Stronger. Fits this Mac's memory.",
        None,
    ),
    row(
        Tier::Fast,
        0,
        "qwen3.5:0.8b",
        1.0,
        "Tiny and quick. Summaries and short writing.",
        None,
    ),
    row(
        Tier::Fast,
        12,
        "granite4.2:3b",
        2.0,
        "Quick and capable for summaries.",
        None,
    ),
    row(
        Tier::Decision,
        0,
        "granite4.2:3b",
        2.0,
        "Short, reliable choices.",
        None,
    ),
];

/// The pick for `tier` on a Mac with `ram_bytes` (16 GB when unknown).
pub fn pick(tier: Tier, ram_bytes: Option<u64>) -> Pick {
    let gb = ram_bytes.map_or(16, |r| r / GIB);
    let r = TABLE
        .iter()
        .filter(|r| r.tier == tier && r.min_gb <= gb)
        .max_by_key(|r| r.min_gb)
        .or_else(|| TABLE.iter().find(|r| r.tier == tier))
        .unwrap_or(&FALLBACK);
    Pick {
        tier,
        model: r.model,
        size_gb: r.size_gb,
        blurb: r.blurb,
        note: r.note,
    }
}

/// All three picks.
pub fn picks(ram_bytes: Option<u64>) -> Vec<Pick> {
    Tier::ALL.iter().map(|t| pick(*t, ram_bytes)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mac_gets_all_three_tiers() {
        for gb in [4, 8, 16, 32, 64, 128] {
            let p = picks(Some(gb * GIB));
            assert_eq!(p.len(), 3);
            assert!(p.iter().all(|x| !x.model.is_empty()));
        }
        assert_eq!(picks(None).len(), 3);
    }

    #[test]
    fn the_agent_tier_grows_with_memory_and_says_so_when_modest() {
        assert_eq!(pick(Tier::Agent, Some(8 * GIB)).model, "granite4.2:3b");
        assert!(pick(Tier::Agent, Some(8 * GIB)).note.is_some());
        assert_eq!(pick(Tier::Agent, Some(16 * GIB)).model, "qwen3.5:9b");
        assert_eq!(pick(Tier::Agent, Some(64 * GIB)).model, "qwen3.6:27b");
        assert!(pick(Tier::Agent, Some(64 * GIB)).note.is_none());
    }

    #[test]
    fn small_models_do_the_fast_and_decision_tiers() {
        assert!(pick(Tier::Decision, Some(16 * GIB)).size_gb <= 2.0);
        assert!(pick(Tier::Fast, Some(8 * GIB)).size_gb <= 2.0);
    }

    #[test]
    fn only_the_fast_tier_may_lack_tools() {
        assert!(Tier::Agent.needs_tools() && Tier::Decision.needs_tools());
        assert!(!Tier::Fast.needs_tools());
    }
}
