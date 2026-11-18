//! The starter model first run offers, sized to this Mac's memory. Data, not
//! logic: model tags change often, so edit this table (and check each tag
//! with a real tool-use run) rather than the code around it. Tags come from
//! docs/research/local-models.md §7 and are not yet verified by a tern eval.

use serde::Serialize;

const GIB: u64 = 1 << 30;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Starter {
    pub model: &'static str,
    /// Download size.
    pub size_gb: f32,
    /// One short line.
    pub blurb: &'static str,
}

/// (Mac memory in GB at least, starter), smallest first.
const TABLE: &[(u64, Starter)] = &[
    (
        0,
        Starter {
            model: "granite4.2:3b",
            size_gb: 2.0,
            blurb: "Small and fast. Light tasks on this Mac.",
        },
    ),
    (
        12,
        Starter {
            model: "qwen3.5:9b",
            size_gb: 6.6,
            blurb: "Good all-round for everyday tasks.",
        },
    ),
    (
        24,
        Starter {
            model: "qwen3.6:27b",
            size_gb: 17.0,
            blurb: "Stronger. Fits this Mac's memory.",
        },
    ),
];

/// The largest starter that fits `ram_bytes`; the 12 GB one when unknown.
pub fn starter_for(ram_bytes: Option<u64>) -> Starter {
    let gb = ram_bytes.map_or(16, |r| r / GIB);
    TABLE
        .iter()
        .rev()
        .find(|(min, _)| gb >= *min)
        .map_or_else(|| TABLE[0].1.clone(), |(_, s)| s.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_memory() {
        assert_eq!(starter_for(Some(8 * GIB)).model, "granite4.2:3b");
        assert_eq!(starter_for(Some(16 * GIB)).model, "qwen3.5:9b");
        assert_eq!(starter_for(Some(32 * GIB)).model, "qwen3.6:27b");
        assert_eq!(starter_for(Some(128 * GIB)).model, "qwen3.6:27b");
        assert_eq!(starter_for(None).model, "qwen3.5:9b");
    }
}
