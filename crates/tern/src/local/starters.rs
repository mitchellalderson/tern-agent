//! The starter model first run offers: the agent-tier pick for this Mac
//! ([`super::catalog`]).

use serde::Serialize;

use super::catalog::{Tier, pick};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Starter {
    pub model: &'static str,
    /// Download size.
    pub size_gb: f32,
    /// One short line.
    pub blurb: &'static str,
}

/// The largest agent model that fits `ram_bytes` (16 GB when unknown).
pub fn starter_for(ram_bytes: Option<u64>) -> Starter {
    let p = pick(Tier::Agent, ram_bytes);
    Starter {
        model: p.model,
        size_gb: p.size_gb,
        blurb: p.blurb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn picks_by_memory() {
        assert_eq!(starter_for(Some(8 * GIB)).model, "granite4.2:3b");
        assert_eq!(starter_for(Some(16 * GIB)).model, "qwen3.5:9b");
        assert_eq!(starter_for(Some(32 * GIB)).model, "qwen3.6:27b");
        assert_eq!(starter_for(Some(128 * GIB)).model, "qwen3.6:27b");
        assert_eq!(starter_for(None).model, "qwen3.5:9b");
    }
}
