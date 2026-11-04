//! Capped profile files and the memory flush before compaction
//! (docs/personal-agent-tooling-spec.md §7, T5a).
//!
//! `user.md` (about the user) and `memory.md` (core facts) are injected
//! into every system prompt, each within a token cap. Writes through
//! `profile_edit` that would exceed a cap are refused with a request to
//! consolidate; a file edited by hand past its cap is cut, with a visible
//! note, so the prompt never carries more than the cap.
//!
//! Before a compaction step, one silent model call over the span about to
//! be summarized may save durable facts with `memory_save`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tern_store::RunRow;

use crate::home::TernHome;
use crate::types::Usage;

/// `[memory]` in config.toml.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    /// Token cap for `user.md` as injected.
    pub user_md_tokens: u64,
    /// Token cap for `memory.md` as injected.
    pub memory_md_tokens: u64,
    /// Let the agent save durable facts before older context is summarized.
    pub flush: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            user_md_tokens: 1500,
            memory_md_tokens: 1500,
            flush: true,
        }
    }
}

/// One of the capped files injected into the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileFile {
    User,
    Memory,
}

impl ProfileFile {
    pub const ALL: [ProfileFile; 2] = [ProfileFile::User, ProfileFile::Memory];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" | "user.md" => Some(Self::User),
            "memory" | "memory.md" => Some(Self::Memory),
            _ => None,
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Self::User => "user.md",
            Self::Memory => "memory.md",
        }
    }

    /// The config key that sets its cap.
    pub fn cap_key(self) -> &'static str {
        match self {
            Self::User => "user_md_tokens",
            Self::Memory => "memory_md_tokens",
        }
    }

    pub fn path(self, home: &TernHome) -> PathBuf {
        match self {
            Self::User => home.user_path(),
            Self::Memory => home.memory_path(),
        }
    }

    pub fn cap(self, cfg: &MemoryConfig) -> u64 {
        match self {
            Self::User => cfg.user_md_tokens,
            Self::Memory => cfg.memory_md_tokens,
        }
    }
}

/// Rough token count, chars / 4, as compaction estimates it.
pub fn tokens(text: &str) -> u64 {
    text.len().div_ceil(4) as u64
}

/// What of a file's text goes into the prompt: HTML comments (the
/// templates' placeholders) removed, trimmed.
pub fn injectable(raw: &str) -> String {
    crate::prompt::strip_html_comments(raw).trim().to_owned()
}

/// A profile file's size against its cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub file: ProfileFile,
    /// `None` when the file doesn't exist.
    pub tokens: Option<u64>,
    pub cap: u64,
}

impl FileStatus {
    pub fn over(&self) -> bool {
        self.tokens.is_some_and(|t| t > self.cap)
    }
}

/// Size of each profile file, for `tern doctor`.
pub fn statuses(home: &TernHome, cfg: &MemoryConfig) -> Vec<FileStatus> {
    ProfileFile::ALL
        .into_iter()
        .map(|file| FileStatus {
            file,
            tokens: std::fs::read_to_string(file.path(home))
                .ok()
                .map(|t| tokens(&injectable(&t))),
            cap: file.cap(cfg),
        })
        .collect()
}

/// The file's text as it goes into the prompt, never over `cap` tokens.
/// `None` when it is missing or empty. An over-cap file is cut at a line
/// boundary and ends with a note asking for consolidation.
pub fn read_capped(home: &TernHome, file: ProfileFile, cap: u64) -> Option<String> {
    let raw = std::fs::read_to_string(file.path(home)).ok()?;
    let text = injectable(&raw);
    (!text.is_empty()).then(|| capped(&text, file, cap))
}

pub fn capped(text: &str, file: ProfileFile, cap: u64) -> String {
    if tokens(text) <= cap {
        return text.to_owned();
    }
    let note = format!(
        "\n\n[{} is over its {cap}-token cap, so the rest is not shown. Consolidate it with \
         `profile_edit` (mode \"replace\"): merge duplicates, drop what's stale, and move \
         details to `memory_save`.]",
        file.file_name()
    );
    let budget = (cap as usize * 4).saturating_sub(note.len());
    let mut cut = budget.min(text.len());
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    // Prefer ending on a whole line.
    if let Some(nl) = text[..cut].rfind('\n') {
        cut = nl;
    }
    let mut out = text[..cut].trim_end().to_owned();
    out.push_str(&note);
    if tokens(&out) > cap {
        // A cap too small for the note itself: the note alone, cut.
        let mut end = (cap as usize * 4).min(note.trim_start().len());
        let note = note.trim_start();
        while !note.is_char_boundary(end) {
            end -= 1;
        }
        return note[..end].to_owned();
    }
    out
}

/// Why a write to a profile file is refused, or `None` if it fits.
pub fn over_cap_error(
    file: ProfileFile,
    cfg: &MemoryConfig,
    new_text: &str,
    current: &str,
) -> Option<String> {
    let cap = file.cap(cfg);
    let t = tokens(&injectable(new_text));
    (t > cap).then(|| {
        let mut msg = format!(
            "not saved: {} would be about {t} tokens, over its cap of {cap} ([memory].{}). \
             Consolidate it first: rewrite the whole file shorter with mode \"replace\" (merge \
             duplicates, drop what's stale or no longer true, keep only what every conversation \
             needs), and save long-tail details with `memory_save`, where they stay searchable.",
            file.file_name(),
            file.cap_key(),
        );
        if current.trim().is_empty() {
            msg.push_str(" The file is empty now; save less at once.");
        } else {
            msg.push_str(&format!(
                "\n\nCurrent {}:\n\n{}",
                file.file_name(),
                current.trim()
            ));
        }
        msg
    })
}

// ---- flush before compaction ----

/// The tool a flush may call.
pub const FLUSH_TOOL: &str = "memory_save";

/// Saves one flush may make; further calls are ignored.
pub const MAX_FLUSH_SAVES: usize = 10;

pub const FLUSH_MAX_TOKENS: u32 = 1024;

/// System prompt for flush calls. Exported so test providers can recognize
/// these requests.
pub const FLUSH_SYSTEM_PROMPT: &str = "You maintain an AI agent's long-term memory. Part of a conversation between the user and the agent is about to be summarized and dropped from the agent's context. Before it goes, save the durable facts in it that will matter in future conversations: the user's preferences, decisions made, people and how they relate to the user, commitments and deadlines, and ongoing projects. Call `memory_save` once per fact, written so it makes sense on its own later, with a tag or two. Don't save what is temporary, only relevant to the task at hand, already recorded in files, or secret (passwords, keys, tokens). Most of the time there is little or nothing worth saving: then call no tools and reply NOTHING.";

pub fn flush_request(transcript: &str) -> crate::types::Message {
    crate::types::Message::user_text(format!(
        "This part of the conversation is about to leave the agent's context. Save its durable facts with `memory_save`, or reply NOTHING.\n\n{transcript}"
    ))
}

/// Whether a run flushes before compacting: only top-level runs the user
/// is in. Sub-agents report to their parent, which keeps what matters, and
/// scheduled runs are unattended (spec §7, T5a).
pub fn flush_applies(run: &RunRow) -> bool {
    run.parent_run_id.is_none() && run.trigger != "cron"
}

/// Whether a run with a token budget can afford a flush over `transcript`
/// after using `used`. Runs without a budget always can.
pub fn flush_affordable(run: &RunRow, used: &Usage, transcript: &str) -> bool {
    let Some(budget) = run.budget_tokens else {
        return true;
    };
    let used =
        used.input_tokens + used.output_tokens + used.cache_read_tokens + used.cache_write_tokens;
    let cost = tokens(transcript) + tokens(FLUSH_SYSTEM_PROMPT) + u64::from(FLUSH_MAX_TOKENS);
    used.saturating_add(cost) < budget
}

/// Journal input for a flush step.
#[derive(Debug, Serialize)]
pub struct FlushInput {
    pub cover_through: i64,
    pub transcript_digest: String,
}

/// Journal output for a flush step. A flush never fails its run: errors
/// are recorded here and compaction goes ahead.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlushOutput {
    /// Results of the saves, in order (`saved memory #N` or an error).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved: Vec<String>,
    /// Why the flush stopped early or saved nothing unexpectedly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub usage: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_files_are_injected_whole() {
        assert_eq!(capped("Name: Sam", ProfileFile::User, 10), "Name: Sam");
    }

    #[test]
    fn over_cap_text_is_cut_on_a_line_with_a_note_and_stays_under_the_cap() {
        let text: String = (0..200).map(|i| format!("- fact number {i}\n")).collect();
        let out = capped(text.trim(), ProfileFile::Memory, 200);
        assert!(tokens(&out) <= 200, "{}", tokens(&out));
        assert!(out.contains("memory.md is over its 200-token cap"), "{out}");
        let body = out.split("\n\n[").next().unwrap();
        assert!(body.ends_with(|c: char| c.is_ascii_digit()), "{body:?}");
        assert!(body.starts_with("- fact number 0\n"));
    }

    #[test]
    fn tiny_caps_and_multibyte_text_never_overflow_or_panic() {
        let text = "é".repeat(500);
        for cap in [0, 1, 5, 40, 100] {
            let out = capped(&text, ProfileFile::User, cap);
            assert!(tokens(&out) <= cap, "cap {cap}: {}", tokens(&out));
        }
    }

    #[test]
    fn writes_over_the_cap_are_refused_with_the_current_text() {
        let cfg = MemoryConfig {
            memory_md_tokens: 10,
            ..MemoryConfig::default()
        };
        assert!(over_cap_error(ProfileFile::Memory, &cfg, "short", "").is_none());
        let err = over_cap_error(ProfileFile::Memory, &cfg, &"x".repeat(80), "old fact").unwrap();
        assert!(
            err.contains("cap of 10 ([memory].memory_md_tokens)"),
            "{err}"
        );
        assert!(err.contains("Current memory.md:\n\nold fact"), "{err}");
        // Comments don't count: they are never injected.
        let commented = format!("<!-- {} -->short", "x".repeat(200));
        assert!(over_cap_error(ProfileFile::Memory, &cfg, &commented, "").is_none());
    }

    #[test]
    fn statuses_report_missing_and_over_cap_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path());
        std::fs::write(home.user_path(), "y".repeat(400)).unwrap();
        let cfg = MemoryConfig {
            user_md_tokens: 50,
            ..MemoryConfig::default()
        };
        let s = statuses(&home, &cfg);
        assert_eq!(s[0].tokens, Some(100));
        assert!(s[0].over());
        assert_eq!(s[1].tokens, None);
        assert!(!s[1].over());
        let injected = read_capped(&home, ProfileFile::User, 50).unwrap();
        assert!(tokens(&injected) <= 50);
    }

    fn run(trigger: &str, parent: Option<&str>, budget: Option<u64>) -> RunRow {
        RunRow {
            id: "r".into(),
            session_id: "s".into(),
            status: "running".into(),
            trigger: trigger.into(),
            model: None,
            system_prompt: None,
            input_message_id: None,
            started_at: 0,
            updated_at: 0,
            finished_at: None,
            error: None,
            parent_run_id: parent.map(Into::into),
            parent_call_id: None,
            label: None,
            depth: 0,
            tool_scope: None,
            budget_tokens: budget,
            max_iterations: None,
            quiet: false,
            input_taint: None,
            taint: None,
        }
    }

    #[test]
    fn flush_is_for_the_users_own_runs_within_budget() {
        assert!(flush_applies(&run("user", None, None)));
        assert!(flush_applies(&run("telegram", None, None)));
        assert!(!flush_applies(&run("cron", None, None)));
        assert!(!flush_applies(&run("subagent", Some("p"), None)));

        let used = Usage {
            input_tokens: 1000,
            ..Usage::default()
        };
        assert!(flush_affordable(&run("user", None, None), &used, "t"));
        assert!(!flush_affordable(
            &run("user", None, Some(1500)),
            &used,
            "t"
        ));
        assert!(flush_affordable(
            &run("user", None, Some(10_000)),
            &used,
            "t"
        ));
    }
}
