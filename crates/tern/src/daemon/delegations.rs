//! Work an agent did for another (agent profiles §6.1): a `delegated` post
//! in the delegate's DM points at it, and its side panel lists it.
//!
//! A **batch** is every sub-agent one parent run delegated to the same
//! agent: one post (key `delegated:<parent run>:<agent>`), rewritten as
//! its children start and finish. Its `session_id` is the parent's thread,
//! where the sub-runs live, so *Open* shows them there; a reply under it
//! starts (then continues) a side thread in the DM as the delegate.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tern_core::models::Change;
use tern_store::{NewPost, RunRow, Store};

use super::manager::Manager;

/// The post kind.
pub const KIND: &str = "delegated";

/// One batch, as the post and the side panel describe it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub parent_run_id: String,
    /// The parent's thread, where the sub-runs show.
    pub session_id: String,
    pub agent_id: String,
    /// Who asked: `Home`, another agent's name, `#channel`.
    pub caller: String,
    /// The first task's first line.
    pub brief: String,
    pub tasks: usize,
    pub running: usize,
    pub failed: usize,
    /// Unix ms.
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tainted: bool,
}

impl Batch {
    /// `running`, `failed` (any child did) or `done`.
    pub fn status(&self) -> &'static str {
        if self.running > 0 {
            "running"
        } else if self.failed > 0 {
            "failed"
        } else {
            "done"
        }
    }

    /// The post's text: "Worked for Home · "brief" · 4 tasks · 6m".
    pub fn text(&self) -> String {
        let tasks = match self.tasks {
            1 => "1 task".to_owned(),
            n => format!("{n} tasks"),
        };
        let mut parts = vec![
            format!(
                "{} for {}",
                if self.running > 0 {
                    "Working"
                } else {
                    "Worked"
                },
                self.caller
            ),
            format!("\"{}\"", self.brief),
            tasks,
        ];
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        match self.finished_at.filter(|_| self.running == 0) {
            Some(end) => parts.push(duration(end - self.started_at)),
            None => parts.push(format!("{} running", self.running)),
        }
        parts.join(" · ")
    }
}

fn duration(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", (s + 30) / 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

/// The post key of a batch.
pub fn key(parent_run: &str, agent: &str) -> String {
    format!("{KIND}:{parent_run}:{agent}")
}

/// Who a run in `session` was asked by, as the delegate's DM says it.
fn caller(store: &Store, session: &str) -> String {
    let Ok(Some(c)) = store.session_channel(session) else {
        return "another agent".into();
    };
    if c.id == tern_store::HOME {
        return "Home".into();
    }
    match (&c.kind[..], &c.agent_id) {
        ("main", Some(a)) => store
            .agent(a)
            .ok()
            .flatten()
            .map_or_else(|| c.title.clone(), |a| a.name),
        _ => format!("#{}", c.slug),
    }
}

/// The batch `parent` delegated to `agent`, from its children now.
pub fn batch(store: &Store, parent: &RunRow, agent: &str) -> anyhow::Result<Option<Batch>> {
    let kids: Vec<RunRow> = store
        .children(&parent.id)?
        .into_iter()
        .filter(|k| store.session_agent(&k.session_id).ok().as_deref() == Some(agent))
        .collect();
    let Some(first) = kids.first() else {
        return Ok(None);
    };
    let brief = first
        .input_message_id
        .map(|id| store.message_text(id))
        .transpose()?
        .flatten()
        .unwrap_or_default();
    let brief: String = brief
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    let running = kids
        .iter()
        .filter(|k| matches!(k.status.as_str(), "queued" | "running"))
        .count();
    let failed = kids.iter().filter(|k| k.status == "failed").count();
    let finished_at = (running == 0)
        .then(|| kids.iter().filter_map(|k| k.finished_at).max())
        .flatten();
    Ok(Some(Batch {
        parent_run_id: parent.id.clone(),
        session_id: parent.session_id.clone(),
        agent_id: agent.to_owned(),
        caller: caller(store, &parent.session_id),
        brief,
        tasks: kids.len(),
        running,
        failed,
        started_at: kids
            .iter()
            .map(|k| k.started_at)
            .min()
            .unwrap_or(first.started_at),
        finished_at,
        tainted: kids.iter().any(|k| k.taint.is_some()),
    }))
}

/// The batch a `delegated` post points at.
pub fn batch_of_post(store: &Store, post: &tern_store::PostRow) -> Option<Batch> {
    if post.kind != KIND {
        return None;
    }
    let parent = store.run(post.run_id.as_deref()?).ok()??;
    let agent = post.author_id.split('/').next()?;
    batch(store, &parent, agent).ok().flatten()
}

impl Manager {
    /// Write or rewrite the pointer for the batch `run_id` belongs to, if
    /// it is a delegated sub-agent (its session's agent isn't its
    /// parent's). Called as the run starts and when it ends.
    pub(super) fn delegation_changed(&self, run_id: &str) {
        // Read the batch and write the post as one step.
        let _guard = self
            .delegation_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Err(e) = self.write_delegation(run_id) {
            tracing::warn!(run = %run_id, error = %e, "writing a delegation post failed");
        }
    }

    fn write_delegation(&self, run_id: &str) -> anyhow::Result<()> {
        let store = &self.rt.store;
        let Some(run) = store.run(run_id)? else {
            return Ok(());
        };
        let Some(parent) = run
            .parent_run_id
            .as_deref()
            .map(|p| store.run(p))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        let agent = store.session_agent(&run.session_id)?;
        if agent == store.session_agent(&parent.session_id)? {
            return Ok(());
        }
        let Some(dm) = store.agent_channel(&agent)? else {
            return Ok(());
        };
        // Work done from the delegate's own DM is already there.
        if store.session_channel(&parent.session_id)?.map(|c| c.id) == Some(dm.id.clone()) {
            return Ok(());
        }
        let Some(b) = batch(store, &parent, &agent)? else {
            return Ok(());
        };
        let name = store
            .agent(&agent)?
            .map_or_else(|| agent.clone(), |a| a.name);
        let key = key(&parent.id, &agent);
        let text = b.text();
        let (row, created) = store.add_post(&NewPost {
            channel_id: &dm.id,
            session_id: Some(&parent.session_id),
            run_id: Some(&parent.id),
            author_kind: "agent",
            author_id: &agent,
            author: &name,
            kind: KIND,
            text: &text,
            tainted: b.tainted,
            dedupe_key: Some(&key),
            ..NewPost::default()
        })?;
        if created {
            self.publish(super::channels::view(store, row));
            return Ok(());
        }
        if row.text != text || row.tainted != b.tainted {
            store.update_post_text(row.id, &text, b.tainted)?;
            if let Some(row) = store.post(row.id)? {
                let _ = self.rt.changes.send(Change::PostUpdated {
                    post: Box::new(super::channels::view(store, row)),
                });
            }
        }
        Ok(())
    }

    /// The first message of a reply under a `delegated` post, when it
    /// starts the side thread: what the work was and what came back, so
    /// the delegate continues it.
    pub(super) fn delegation_context(&self, post: &tern_store::PostRow, text: &str) -> String {
        let store = &self.rt.store;
        let Some(b) = batch_of_post(store, post) else {
            return text.to_owned();
        };
        let reports: Vec<String> = store
            .children(&b.parent_run_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|k| store.session_agent(&k.session_id).ok().as_deref() == Some(&b.agent_id))
            .map(|k| {
                let report = super::channels::final_text(store, &k).unwrap_or_default();
                let report: String = report.chars().take(1500).collect();
                format!(
                    "- {} ({}): {}",
                    k.label.unwrap_or_default(),
                    k.status,
                    report.trim()
                )
            })
            .collect();
        format!(
            "[Continuing work you did for {}: \"{}\". What you reported:\n{}]\n\n{text}",
            b.caller,
            b.brief,
            reports.join("\n")
        )
    }

    /// Recent work of `agent` for its side panel: delegation batches (from
    /// their posts) and routine runs, newest first.
    pub fn recent_work(self: &Arc<Self>, agent: &str, limit: usize) -> anyhow::Result<Vec<Work>> {
        let store = &self.rt.store;
        let mut out = Vec::new();
        if let Some(dm) = store.agent_channel(agent)? {
            for p in store.posts(&dm.id, None, 200)? {
                if let Some(b) = batch_of_post(store, &p) {
                    out.push(Work {
                        kind: "delegated".into(),
                        title: b.brief.clone(),
                        caller: Some(b.caller.clone()),
                        status: b.status().into(),
                        session_id: b.session_id.clone(),
                        run_id: b.parent_run_id.clone(),
                        tasks: b.tasks,
                        started_at: b.started_at,
                        finished_at: b.finished_at,
                        post_id: Some(p.id),
                    });
                }
            }
        }
        for s in store
            .schedules()?
            .into_iter()
            .filter(|s| s.agent() == agent)
        {
            for r in store
                .runs_for_session(&s.session_id)?
                .into_iter()
                .rev()
                .take(limit)
            {
                out.push(Work {
                    kind: "routine".into(),
                    title: super::scheduler::title(&s),
                    caller: None,
                    status: match r.status.as_str() {
                        "queued" | "running" => "running",
                        "failed" => "failed",
                        _ => "done",
                    }
                    .into(),
                    session_id: r.session_id.clone(),
                    run_id: r.id.clone(),
                    tasks: 1,
                    started_at: r.started_at,
                    finished_at: r.finished_at,
                    post_id: None,
                });
            }
        }
        out.sort_by_key(|w| std::cmp::Reverse(w.started_at));
        out.truncate(limit);
        Ok(out)
    }
}

/// A line of an agent's recent work (`GET /v1/agents/{id}/work`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Work {
    /// `delegated` or `routine`.
    pub kind: String,
    /// The brief, or the routine's title.
    pub title: String,
    /// Who asked, for delegated work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    /// `running`, `done` or `failed`.
    pub status: String,
    /// Where to open it: the caller's thread, or the routine's session.
    pub session_id: String,
    pub run_id: String,
    pub tasks: usize,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_says_what_and_how_long() {
        let b = Batch {
            parent_run_id: "p".into(),
            session_id: "s".into(),
            agent_id: "tern:research".into(),
            caller: "Home".into(),
            brief: "Size the demand".into(),
            tasks: 4,
            running: 2,
            failed: 0,
            started_at: 0,
            finished_at: None,
            tainted: false,
        };
        assert_eq!(
            b.text(),
            "Working for Home · \"Size the demand\" · 4 tasks · 2 running"
        );
        let done = Batch {
            running: 0,
            failed: 1,
            finished_at: Some(6 * 60 * 1000),
            ..b
        };
        assert_eq!(done.status(), "failed");
        assert_eq!(
            done.text(),
            "Worked for Home · \"Size the demand\" · 4 tasks · 1 failed · 6m"
        );
    }

    /// Children that end together each save their status, then refresh the
    /// post: the last one to write must have read every status.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn children_ending_together_leave_the_final_post() {
        use super::super::manager::Routes;
        use std::sync::{Arc, Barrier};
        use tern_store::{NewMessage, NewRun};

        const KIDS: usize = 12;
        let dir = tempfile::tempdir().unwrap();
        let home = tern_core::TernHome::new(dir.path());
        let rt =
            crate::runtime::Runtime::open(&home, Arc::new(tern_core::approval::AutoDeny)).unwrap();
        let agent = rt
            .create_agent(crate::runtime::NewAgent {
                name: "Scout".into(),
                ..crate::runtime::NewAgent::default()
            })
            .unwrap();
        let store = rt.store.clone();
        let manager = Manager::new(Arc::new(rt), Arc::new(Routes::default()));
        let input = NewMessage {
            role: "user".into(),
            content_json: "[]".into(),
            text: "Find the pricing".into(),
            summary_of: None,
        };
        let mk = |id: &str, session: &str, parent: Option<&str>| {
            store.create_session(session, "test", None).unwrap();
            store
                .create_run(
                    &NewRun {
                        id,
                        session_id: session,
                        trigger: "subagent",
                        model: "mock:mock",
                        system_prompt: "system",
                        parent_run_id: parent,
                        parent_call_id: parent.map(|_| id),
                        label: parent.map(|_| id),
                        depth: u32::from(parent.is_some()),
                        ..NewRun::default()
                    },
                    &input,
                )
                .unwrap();
        };
        for round in 0..20 {
            let parent = format!("p{round}");
            mk(&parent, &format!("sp{round}"), None);
            let kids: Vec<String> = (0..KIDS).map(|k| format!("k{round}-{k}")).collect();
            for kid in &kids {
                let session = format!("s{kid}");
                mk(kid, &session, Some(&parent));
                store.set_session_agent(&session, &agent.id).unwrap();
            }
            for kid in &kids {
                manager.delegation_changed(kid);
            }
            let barrier = Arc::new(Barrier::new(KIDS));
            let threads: Vec<_> = kids
                .iter()
                .enumerate()
                .map(|(i, kid)| {
                    let (m, store, kid, barrier) =
                        (manager.clone(), store.clone(), kid.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        // One of them fails, as a run can.
                        let status = if i == 0 { "failed" } else { "completed" };
                        store.set_run_status(&kid, status, None).unwrap();
                        m.delegation_changed(&kid);
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            let key = key(&parent, &agent.id);
            let dm = store.agent_channel(&agent.id).unwrap().unwrap();
            let (post, _) = store
                .add_post(&tern_store::NewPost {
                    channel_id: &dm.id,
                    dedupe_key: Some(&key),
                    author_kind: "agent",
                    author_id: &agent.id,
                    author: "x",
                    kind: KIND,
                    text: "probe",
                    ..tern_store::NewPost::default()
                })
                .unwrap();
            assert!(
                post.text.starts_with("Worked for Home") && post.text.contains("12 tasks"),
                "round {round}: {}",
                post.text
            );
        }
    }
}
