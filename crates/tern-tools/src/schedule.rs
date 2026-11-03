//! `schedule_create`, `schedule_list`, `schedule_delete` (spec §12.5).
//! The daemon runs due schedules.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::cron::Cron;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;
use tern_store::{ScheduleRow, Store};

use crate::{args, spec};

/// Schedules are tern's own state: a write the host grants.
const POLICY: ToolPolicy = ToolPolicy::granted_write(false);

fn fmt_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%a %Y-%m-%d %H:%M %Z")
                .to_string()
        })
        .unwrap_or_else(|| "?".into())
}

/// Where results go when the caller says "here": the chat this session is
/// attached to, if any.
fn here(store: &Store, session_id: &str) -> Option<String> {
    let s = store.session(session_id).ok().flatten()?;
    match (s.source.as_str(), s.external_ref) {
        ("telegram", Some(chat)) => Some(format!("telegram:{chat}")),
        _ => None,
    }
}

/// Validate and save a schedule with its own `cron` session. `deliver_to`
/// is already resolved (`None` = keep results in the session).
pub fn create_schedule(
    store: &Store,
    cron: &str,
    prompt: &str,
    deliver_to: Option<String>,
) -> Result<ScheduleRow, String> {
    let cron: Cron = cron
        .parse()
        .map_err(|e: tern_core::cron::CronError| e.to_string())?;
    if prompt.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    if let Some(t) = &deliver_to {
        check_deliver_to(t)?;
    }
    let next = cron
        .next_after(chrono::Local::now())
        .ok_or("that schedule never fires")?;
    let row = ScheduleRow {
        id: format!("sch_{}", &tern_core::agent::new_id()[20..]),
        cron: cron.to_string(),
        prompt: prompt.trim().to_owned(),
        session_id: tern_core::agent::new_id(),
        deliver_to,
        enabled: true,
        next_run_at: next.timestamp_millis(),
        last_run_at: None,
    };
    store
        .create_session(&row.session_id, "cron", None)
        .and_then(|_| store.create_schedule(&row))
        .map_err(|e| format!("saving schedule: {e}"))?;
    Ok(row)
}

/// A concrete delivery target: `telegram:<chat_id>`.
pub fn check_deliver_to(t: &str) -> Result<(), String> {
    match t.strip_prefix("telegram:") {
        Some(chat) if chat.parse::<i64>().is_ok() => Ok(()),
        _ => Err(format!(
            "deliver_to {t:?} must be \"here\", \"none\", or \"telegram:<chat_id>\""
        )),
    }
}

pub struct ScheduleCreate {
    pub store: Store,
}

#[derive(Deserialize)]
struct CreateArgs {
    cron: String,
    prompt: String,
    deliver_to: Option<String>,
}

#[async_trait]
impl Tool for ScheduleCreate {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "schedule_create",
            "Schedule a prompt to run on a cron schedule (5 fields: minute hour day-of-month month day-of-week, in the user's local time; e.g. \"0 8 * * 1-5\" is weekdays at 08:00). Each run happens in its own session and its final answer is delivered to `deliver_to`: \"here\" (default: this chat, if it's a messaging chat), \"none\", or \"telegram:<chat_id>\". Scheduled runs only happen while `tern daemon` is running.",
            json!({
                "type": "object",
                "properties": {
                    "cron": { "type": "string" },
                    "prompt": { "type": "string", "description": "what to do each time, written as a standalone instruction" },
                    "deliver_to": { "type": "string" }
                },
                "required": ["cron", "prompt"]
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        POLICY
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: CreateArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let deliver_to = match a.deliver_to.as_deref().map(str::trim) {
            None | Some("here") | Some("") => here(&self.store, &ctx.session_id),
            Some("none") => None,
            Some(t) => Some(t.to_owned()),
        };
        let row = match create_schedule(&self.store, &a.cron, &a.prompt, deliver_to.clone()) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(e),
        };
        let (id, cron, next) = (row.id, row.cron, row.next_run_at);
        ToolResult::ok(format!(
            "created schedule {id} ({cron}); next run {}; results go to {}",
            fmt_time(next),
            deliver_to
                .as_deref()
                .unwrap_or("nowhere (check with `tern sessions`)")
        ))
    }
}

pub struct ScheduleList {
    pub store: Store,
}

#[async_trait]
impl Tool for ScheduleList {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "schedule_list",
            "List scheduled prompts.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        match self.store.schedules() {
            Err(e) => ToolResult::error(e.to_string()),
            Ok(list) if list.is_empty() => ToolResult::ok("no schedules"),
            Ok(list) => ToolResult::ok(
                list.iter()
                    .map(|s| {
                        format!(
                            "{} [{}] next {} → {}: {}",
                            s.id,
                            s.cron,
                            fmt_time(s.next_run_at),
                            s.deliver_to.as_deref().unwrap_or("none"),
                            s.prompt
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
}

pub struct ScheduleDelete {
    pub store: Store,
}

#[async_trait]
impl Tool for ScheduleDelete {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "schedule_delete",
            "Delete a schedule by id.",
            json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        POLICY
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let id = raw.get("id").and_then(Value::as_str).unwrap_or_default();
        match self.store.delete_schedule(id) {
            Ok(true) => ToolResult::ok(format!("deleted {id}")),
            Ok(false) => ToolResult::error(format!("no schedule {id:?}")),
            Err(e) => ToolResult::error(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn create_list_delete_and_deliver_here() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_session_with_ref("chat", "telegram", "555", None)
            .unwrap();
        let ctx = ToolCtx {
            workspace: std::env::temp_dir(),
            run_id: "r".into(),
            session_id: "chat".into(),
            cancel: CancellationToken::new(),
        };
        let create = ScheduleCreate {
            store: store.clone(),
        };
        let bad = create
            .call(&ctx, json!({"cron": "0 8 * *", "prompt": "x"}))
            .await;
        assert!(bad.is_error && bad.content.contains("5 fields"));
        let r = create
            .call(&ctx, json!({"cron": "0 8 * * 1-5", "prompt": "Summarize my unread GitHub notifications"}))
            .await;
        assert!(
            !r.is_error && r.content.contains("telegram:555"),
            "{}",
            r.content
        );
        let s = &store.schedules().unwrap()[0];
        assert_eq!(s.deliver_to.as_deref(), Some("telegram:555"));
        assert_eq!(
            store.session(&s.session_id).unwrap().unwrap().source,
            "cron"
        );
        assert!(s.next_run_at > tern_store::now_ms());

        let listed = ScheduleList {
            store: store.clone(),
        }
        .call(&ctx, json!({}))
        .await
        .content;
        assert!(
            listed.contains("0 8 * * 1-5") && listed.contains("GitHub"),
            "{listed}"
        );
        let del = ScheduleDelete {
            store: store.clone(),
        };
        assert!(!del.call(&ctx, json!({"id": s.id})).await.is_error);
        assert!(del.call(&ctx, json!({"id": s.id})).await.is_error);
    }
}
