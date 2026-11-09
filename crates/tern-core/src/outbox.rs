//! The outbox (tooling spec §5.2): a `write` or `destructive` call nobody
//! approved in time is queued here instead of denied, and the run carries
//! on. Approving it later executes the call exactly once, through an
//! `outbox_exec` step on a small system run in the same session.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tern_store::OutboxRow;

/// `runs.trigger` of the system run that executes an approved item.
pub const TRIGGER: &str = "outbox";

/// `agent_mail.kind` of tern's notes to a run still working when one of
/// its items is decided.
pub const MAIL_KIND: &str = "outbox";

/// Tools that write one file named by `path`. A queued call to one of these
/// remembers the file's state, so approving it later can tell the file
/// changed in between.
pub const FILE_TOOLS: &[&str] = &["write_file", "edit_file"];

/// The hash recorded for a file that doesn't exist.
pub const ABSENT: &str = "absent";

/// The file a queued call would write, resolved against `cwd` (the run's
/// folder), or `None` for any other tool.
pub fn file_target(tool: &str, input: &Value, cwd: &Path) -> Option<PathBuf> {
    if !FILE_TOOLS.contains(&tool) {
        return None;
    }
    let raw = input.get("path")?.as_str()?;
    Some(crate::workspace::canonicalize_lenient(&cwd.join(raw)))
}

/// A file's content hash, or [`ABSENT`] when it can't be read.
pub fn file_hash(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => blake3::hash(&bytes).to_hex().to_string(),
        Err(_) => ABSENT.to_owned(),
    }
}

/// Whether the file a queued edit was meant for has changed since (edited,
/// deleted or created).
pub fn file_changed(file_path: Option<&str>, file_hash_then: Option<&str>) -> bool {
    match (file_path, file_hash_then) {
        (Some(p), Some(h)) => file_hash(Path::new(p)) != h,
        _ => false,
    }
}

/// `a.md` for `/work/notes/a.md`.
pub fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// The one-line reason a queued edit was skipped.
pub fn stale_message(path: &str) -> String {
    format!("{} changed since this was queued.", file_name(path))
}

/// One message for edits skipped together, newest batch of one file.
pub fn stale_summary(path: &str, n: usize) -> String {
    let file = file_name(path);
    if n == 1 {
        format!("A queued edit to {file} was skipped: the file changed.")
    } else {
        format!("{n} queued edits to {file} were skipped: the file changed.")
    }
}

/// What the model is told when its call goes to the outbox.
pub fn deferred_message(id: i64) -> String {
    format!(
        "queued for approval as outbox #{id}: nobody was there to approve it, so it hasn't run. \
         Continue with anything that doesn't depend on it, and say in your answer that it is waiting \
         for the user. If you are still running when the user decides, you'll be told."
    )
}

/// The outbox id in a [`deferred_message`], for a script's `queued` result.
pub fn deferred_id(message: &str) -> Option<i64> {
    let rest = message.strip_prefix("queued for approval as outbox #")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The note a running run gets when an item is decided.
pub fn decided_note(item: &OutboxRow, result: Option<&str>) -> String {
    match item.state.as_str() {
        "done" | "failed" => format!(
            "[outbox #{} was approved and ran: {}]\n{}",
            item.id,
            item.summary,
            result.unwrap_or_default()
        ),
        "rejected" => match item.note.as_deref().filter(|n| !n.trim().is_empty()) {
            Some(note) => format!(
                "[outbox #{} was rejected: {}]\nThe user said: {note}",
                item.id, item.summary
            ),
            None => format!("[outbox #{} was rejected: {}]", item.id, item.summary),
        },
        "stale" => format!(
            "[outbox #{} was skipped, the file changed since it was queued: {}]",
            item.id, item.summary
        ),
        "expired" => format!(
            "[outbox #{} expired without an answer: {}]",
            item.id, item.summary
        ),
        _ => format!("[outbox #{}: {}]", item.id, item.summary),
    }
}

/// An outbox item as the API, Telegram and clients see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: i64,
    pub run_id: String,
    pub session_id: String,
    pub tool: String,
    /// The arguments it would run with (the user's edit, if any).
    pub input: Value,
    pub summary: String,
    pub effect: crate::tool::Effect,
    /// Why the run that queued it was tainted, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taint: Option<String>,
    /// Only a client on this host may approve it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_only: bool,
    /// `pending`, `approved`, `done`, `failed`, `rejected`, `expired` or
    /// `stale`.
    pub state: String,
    /// The file a queued edit targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Its file changed since it was queued, so it can't run: only Discard.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    pub created_at: i64,
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The user changed the arguments before approving.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_run_id: Option<String>,
    /// The tool's result once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<crate::tool::ToolResult>,
    /// The channel and agent of the session at the top of the run's tree
    /// (agent profiles §3): an agent's DM shows its own items as well as
    /// Home. Filled in by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

impl From<OutboxRow> for OutboxItem {
    fn from(r: OutboxRow) -> Self {
        Self {
            id: r.id,
            input: serde_json::from_str(r.effective_input()).unwrap_or(Value::Null),
            edited: r.edited_input_json.is_some(),
            result: r
                .result_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok()),
            run_id: r.run_id,
            session_id: r.session_id,
            tool: r.tool,
            summary: r.summary,
            effect: crate::tool::Effect::parse(&r.effect).unwrap_or_default(),
            taint: r.taint,
            local_only: r.local_only,
            stale: r.state == "stale"
                || (r.state == "pending"
                    && file_changed(r.file_path.as_deref(), r.file_hash.as_deref())),
            file: r.file_path,
            state: r.state,
            created_at: r.created_at,
            expires_at: r.expires_at,
            decided_at: r.decided_at,
            note: r.note,
            exec_run_id: r.exec_run_id,
            channel_id: None,
            agent_id: None,
        }
    }
}

impl OutboxItem {
    /// `write · gmail_send to bob@…`, as approval prompts show a call.
    pub fn headline(&self) -> String {
        format!("{} · {}", self.effect, self.summary)
    }
}

#[cfg(test)]
mod deferred_id_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_target_resolves_only_file_tools() {
        let dir = tempfile::tempdir().unwrap();
        let input = json!({ "path": "a.md" });
        let t = file_target("edit_file", &input, dir.path()).unwrap();
        assert!(t.ends_with("a.md"));
        assert_eq!(file_target("write_file", &input, dir.path()), Some(t));
        assert_eq!(file_target("shell", &input, dir.path()), None);
        assert_eq!(file_target("edit_file", &json!({}), dir.path()), None);
    }

    #[test]
    fn hash_captures_content_and_absence() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.md");
        assert_eq!(file_hash(&f), ABSENT);
        std::fs::write(&f, "one").unwrap();
        let one = file_hash(&f);
        assert_ne!(one, ABSENT);
        assert_eq!(file_hash(&f), one, "stable");
        std::fs::write(&f, "two").unwrap();
        assert_ne!(file_hash(&f), one);
    }

    #[test]
    fn changed_means_edited_deleted_or_created() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.md");
        let p = f.to_str().unwrap();
        // Created since: it was absent when queued.
        assert!(!file_changed(Some(p), Some(ABSENT)));
        std::fs::write(&f, "one").unwrap();
        assert!(file_changed(Some(p), Some(ABSENT)));
        // Unchanged, then edited, then deleted.
        let h = file_hash(&f);
        assert!(!file_changed(Some(p), Some(&h)));
        std::fs::write(&f, "two").unwrap();
        assert!(file_changed(Some(p), Some(&h)));
        std::fs::remove_file(&f).unwrap();
        assert!(file_changed(Some(p), Some(&h)));
        // Not a file edit: never stale.
        assert!(!file_changed(None, None));
    }

    #[test]
    fn the_messages_are_one_line() {
        assert_eq!(
            stale_message("/w/vs-trigger-outline.md"),
            "vs-trigger-outline.md changed since this was queued."
        );
        assert_eq!(
            stale_summary("/w/vs-trigger-outline.md", 4),
            "4 queued edits to vs-trigger-outline.md were skipped: the file changed."
        );
        assert!(stale_summary("/w/a.md", 1).starts_with("A queued edit to a.md"));
    }

    #[test]
    fn the_id_comes_back_out_of_the_message() {
        assert_eq!(deferred_id(&deferred_message(12)), Some(12));
        assert_eq!(deferred_id("denied: nobody was there"), None);
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;

    fn row(file: &std::path::Path, hash: &str, state: &str) -> OutboxRow {
        OutboxRow {
            id: 1,
            run_id: "r".into(),
            session_id: "s".into(),
            call_id: "c".into(),
            tool: "edit_file".into(),
            input_json: "{}".into(),
            summary: "edit a.md".into(),
            effect: "write".into(),
            taint: None,
            local_only: false,
            state: state.into(),
            created_at: 0,
            expires_at: 0,
            decided_at: None,
            note: None,
            edited_input_json: None,
            exec_run_id: None,
            result_json: None,
            file_path: Some(file.to_string_lossy().into_owned()),
            file_hash: Some(hash.into()),
        }
    }

    #[test]
    fn the_list_shows_a_changed_pending_item_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.md");
        std::fs::write(&f, "one").unwrap();
        let h = file_hash(&f);
        assert!(!OutboxItem::from(row(&f, &h, "pending")).stale);
        std::fs::write(&f, "two").unwrap();
        let item = OutboxItem::from(row(&f, &h, "pending"));
        assert!(item.stale);
        assert_eq!(item.state, "pending");
        // Decided items aren't re-checked; a stale one stays stale.
        assert!(!OutboxItem::from(row(&f, &h, "done")).stale);
        assert!(OutboxItem::from(row(&f, &h, "stale")).stale);
    }
}
