use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use tokio::sync::Notify;

use super::*;
use crate::approval::{AutoApprove, AutoDeny};
use crate::tool::{Tool, ToolPolicy};
use crate::types::{EventStream, StopReason, ToolSpec};

// ---- test doubles ----

#[derive(Clone)]
enum Reply {
    Text(&'static str),
    Tool(&'static str, Value),
}

/// Replies by number of assistant turns so far; can fail the first N calls.
struct Script {
    replies: Vec<Reply>,
    fail_first: AtomicU32,
    fail_status: u16,
    delay: Duration,
    seen: Mutex<Vec<ChatRequest>>,
    summaries: AtomicU32,
}

fn turns_in(text: &str) -> usize {
    text.split("turns=")
        .skip(1)
        .filter_map(|r| {
            r.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<usize>()
                .ok()
        })
        .sum()
}

impl Script {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies,
            fail_first: AtomicU32::new(0),
            fail_status: 500,
            delay: Duration::ZERO,
            seen: Mutex::new(Vec::new()),
            summaries: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl Provider for Script {
    fn id(&self) -> &str {
        "script"
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        self.seen.lock().unwrap().push(req.clone());
        if self.fail_first.load(Ordering::SeqCst) > 0 {
            self.fail_first.fetch_sub(1, Ordering::SeqCst);
            return Err(ProviderError::Http {
                status: self.fail_status,
                body: "boom".into(),
                retry_after: None,
            });
        }
        if !self.delay.is_zero() {
            tokio::select! {
                () = tokio::time::sleep(self.delay) => {}
                () = cancel.cancelled() => return Err(ProviderError::Cancelled),
            }
        }
        if req.system == crate::compaction::SYSTEM_PROMPT {
            self.summaries.fetch_add(1, Ordering::SeqCst);
            let text: String = req.messages.iter().map(|m| m.text()).collect();
            let n = text.matches("### assistant").count() + turns_in(&text);
            let evs = vec![
                Ok(StreamEvent::TextDelta(format!("summary turns={n}"))),
                Ok(StreamEvent::MessageStop {
                    usage: Usage::default(),
                    stop_reason: StopReason::EndTurn,
                }),
            ];
            return Ok(Box::pin(futures::stream::iter(evs)));
        }
        // Count assistant turns, including ones folded into a summary.
        let n = req
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count()
            + req
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .map(|m| turns_in(&m.text()))
                .sum::<usize>();
        let reply = self
            .replies
            .get(n)
            .cloned()
            .unwrap_or(Reply::Text("(exhausted)"));
        let mut evs = vec![];
        let stop_reason = match reply {
            Reply::Text(t) => {
                evs.push(Ok(StreamEvent::TextDelta(t.into())));
                StopReason::EndTurn
            }
            Reply::Tool(name, input) => {
                let id = format!("call_{n}");
                evs.push(Ok(StreamEvent::ToolUseStart {
                    id: id.clone(),
                    name: name.into(),
                }));
                evs.push(Ok(StreamEvent::ToolUseInputDelta {
                    id,
                    partial_json: input.to_string(),
                }));
                StopReason::ToolUse
            }
        };
        evs.push(Ok(StreamEvent::MessageStop {
            usage: Usage {
                input_tokens: crate::compaction::estimate_tokens(
                    &req.system,
                    &req.messages,
                    &req.tools,
                ),
                output_tokens: 2,
                ..Usage::default()
            },
            stop_reason,
        }));
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

/// Counts calls. Optionally blocks until released, announcing when it starts.
struct Counter {
    name: &'static str,
    calls: Arc<AtomicU32>,
    policy: ToolPolicy,
    started: Option<Arc<Notify>>,
    gate: Option<Arc<Notify>>,
}

impl Counter {
    fn new(name: &'static str, idempotent: bool, requires_approval: bool) -> Self {
        Self {
            name,
            calls: Arc::new(AtomicU32::new(0)),
            policy: ToolPolicy {
                idempotent,
                requires_approval,
            },
            started: None,
            gate: None,
        }
    }
}

#[async_trait]
impl Tool for Counter {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "count".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        self.policy
    }
    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(s) = &self.started {
            s.notify_one();
        }
        if let Some(g) = &self.gate {
            g.notified().await;
        }
        ToolResult::ok(format!("count={n}"))
    }
}

struct Harness {
    store: Store,
    session: String,
}

impl Harness {
    fn new() -> Self {
        let store = Store::open_in_memory().unwrap();
        store.create_session("s1", "cli", None).unwrap();
        Self {
            store,
            session: "s1".into(),
        }
    }

    fn agent(
        &self,
        provider: Arc<Script>,
        tools: Vec<Arc<dyn Tool>>,
        approvals: Arc<dyn ApprovalHandler>,
    ) -> Agent {
        self.agent_with_window(provider, tools, approvals, 128_000)
    }

    fn agent_with_window(
        &self,
        provider: Arc<Script>,
        tools: Vec<Arc<dyn Tool>>,
        approvals: Arc<dyn ApprovalHandler>,
        window: u64,
    ) -> Agent {
        let mut reg = ToolRegistry::new();
        for t in tools {
            reg.register(t);
        }
        let p: Arc<dyn Provider> = provider;
        Agent::new(
            Journal::new(self.store.clone()),
            Arc::new(move |_| Ok(p.clone())),
            Arc::new(reg),
            approvals,
            AgentConfig {
                retry_base: Duration::from_millis(1),
                max_iterations: 10,
                default_context_window: window,
                ..AgentConfig::default()
            },
        )
    }

    fn start(&self, agent: &Agent, text: &str) -> String {
        let model: ModelRef = "script:m".parse().unwrap();
        agent
            .start_run(&self.session, text, &model, "system", "user")
            .unwrap()
            .id
    }

    fn kinds(&self, run: &str) -> Vec<String> {
        self.store
            .steps(run)
            .unwrap()
            .into_iter()
            .map(|s| format!("{}:{}", s.kind, s.status))
            .collect()
    }
}

fn last_tool_result(req: &ChatRequest) -> Option<(String, bool)> {
    req.messages
        .iter()
        .rev()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
}

// ---- tests ----

#[tokio::test]
async fn text_reply_completes() {
    let h = Harness::new();
    let agent = h.agent(
        Arc::new(Script::new(vec![Reply::Text("hello!")])),
        vec![],
        Arc::new(AutoApprove),
    );
    let mut events = agent.subscribe();
    let run = h.start(&agent, "hi");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(out.status, RunStatus::Completed);
    assert_eq!(out.final_text, "hello!");
    assert_eq!(h.store.run(&run).unwrap().unwrap().status, "completed");
    assert_eq!(h.store.session_messages("s1").unwrap().len(), 2);
    let mut saw_delta = false;
    while let Ok(ev) = events.try_recv() {
        saw_delta |= matches!(ev, AgentEvent::TextDelta { .. });
    }
    assert!(saw_delta);
}

#[tokio::test]
async fn tool_loop_feeds_results_back() {
    let h = Harness::new();
    let tool = Arc::new(Counter::new("count", false, false));
    let calls = tool.calls.clone();
    let script = Arc::new(Script::new(vec![
        Reply::Tool("count", json!({})),
        Reply::Text("done"),
    ]));
    let agent = h.agent(script.clone(), vec![tool], Arc::new(AutoApprove));
    let run = h.start(&agent, "go");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        (out.status, out.final_text.as_str()),
        (RunStatus::Completed, "done")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.kinds(&run),
        ["llm:completed", "tool:completed", "llm:completed"]
    );
    let second = &script.seen.lock().unwrap()[1];
    assert_eq!(last_tool_result(second), Some(("count=1".into(), false)));
    assert_eq!(second.system, "system");
    assert_eq!(second.tools[0].name, "count");
}

#[tokio::test]
async fn resume_replays_completed_tools_without_rerunning() {
    let h = Harness::new();
    let tool = Arc::new(Counter::new("count", false, false));
    let calls = tool.calls.clone();
    let replies = vec![
        Reply::Tool("count", json!({})),
        Reply::Tool("count", json!({"n": 2})),
        Reply::Text("done"),
    ];

    // Create the run with any agent; the first drive uses a provider whose
    // second call fails permanently.
    let run = h.start(
        &h.agent(Arc::new(Script::new(vec![])), vec![], Arc::new(AutoApprove)),
        "go",
    );
    {
        // Drive once with a provider that dies on its second call.
        let p = Arc::new(Script::new(replies.clone()));
        struct FailSecond(Arc<Script>, AtomicU32);
        #[async_trait]
        impl Provider for FailSecond {
            fn id(&self) -> &str {
                "f"
            }
            async fn stream(
                &self,
                r: ChatRequest,
                c: CancellationToken,
            ) -> Result<EventStream, ProviderError> {
                if self.1.fetch_add(1, Ordering::SeqCst) == 1 {
                    return Err(ProviderError::Http {
                        status: 400,
                        body: "bad".into(),
                        retry_after: None,
                    });
                }
                self.0.stream(r, c).await
            }
        }
        let fp: Arc<dyn Provider> = Arc::new(FailSecond(p, AtomicU32::new(0)));
        let mut reg = ToolRegistry::new();
        reg.register(tool.clone());
        let a = Agent::new(
            Journal::new(h.store.clone()),
            Arc::new(move |_| Ok(fp.clone())),
            Arc::new(reg),
            Arc::new(AutoApprove),
            AgentConfig {
                retry_base: Duration::from_millis(1),
                ..AgentConfig::default()
            },
        );
        let out = a.drive(&run, CancellationToken::new()).await.unwrap();
        assert_eq!(out.status, RunStatus::Failed);
        assert!(out.error.unwrap().contains("400"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.kinds(&run),
        ["llm:completed", "tool:completed", "llm:failed"]
    );

    let ok = Arc::new(Script::new(replies));
    let agent = h.agent(ok.clone(), vec![tool], Arc::new(AutoApprove));
    let mut events = agent.subscribe();
    let out = agent.resume(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        (out.status, out.final_text.as_str()),
        (RunStatus::Completed, "done")
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "first tool replayed, second ran live"
    );
    // The replayed first model call was not re-sent.
    assert_eq!(ok.seen.lock().unwrap().len(), 2);
    let mut replayed_tool = false;
    while let Ok(ev) = events.try_recv() {
        if let AgentEvent::ToolFinished { replayed: true, .. } = ev {
            replayed_tool = true;
        }
    }
    assert!(replayed_tool);
}

#[tokio::test]
async fn crash_during_non_idempotent_tool_is_not_rerun() {
    let h = Harness::new();
    let started = Arc::new(Notify::new());
    let mut tool = Counter::new("count", false, false);
    tool.started = Some(started.clone());
    tool.gate = Some(Arc::new(Notify::new())); // never released
    let tool = Arc::new(tool);
    let calls = tool.calls.clone();
    let replies = vec![Reply::Tool("count", json!({})), Reply::Text("checked")];

    let agent = Arc::new(h.agent(
        Arc::new(Script::new(replies.clone())),
        vec![tool],
        Arc::new(AutoApprove),
    ));
    let run = h.start(&agent, "go");
    let task = {
        let (agent, run) = (agent.clone(), run.clone());
        tokio::spawn(async move { agent.drive(&run, CancellationToken::new()).await })
    };
    started.notified().await;
    task.abort(); // the "crash": the in-flight step is left `started`
    let _ = task.await;
    assert_eq!(h.kinds(&run), ["llm:completed", "tool:started"]);

    let fresh = Arc::new(Counter::new("count", false, false));
    let fresh_calls = fresh.calls.clone();
    let script = Arc::new(Script::new(replies));
    let agent = h.agent(script.clone(), vec![fresh], Arc::new(AutoApprove));
    let out = agent.resume(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(out.status, RunStatus::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fresh_calls.load(Ordering::SeqCst),
        0,
        "non-idempotent tool must not re-run"
    );
    let req = &script.seen.lock().unwrap()[0];
    assert_eq!(
        last_tool_result(req),
        Some((INTERRUPTED_TOOL_MESSAGE.into(), true))
    );
}

#[tokio::test]
async fn crash_during_idempotent_tool_reruns_it() {
    let h = Harness::new();
    let started = Arc::new(Notify::new());
    let mut tool = Counter::new("count", true, false);
    tool.started = Some(started.clone());
    tool.gate = Some(Arc::new(Notify::new()));
    let replies = vec![Reply::Tool("count", json!({})), Reply::Text("ok")];
    let agent = Arc::new(h.agent(
        Arc::new(Script::new(replies.clone())),
        vec![Arc::new(tool)],
        Arc::new(AutoApprove),
    ));
    let run = h.start(&agent, "go");
    let task = {
        let (agent, run) = (agent.clone(), run.clone());
        tokio::spawn(async move { agent.drive(&run, CancellationToken::new()).await })
    };
    started.notified().await;
    task.abort();
    let _ = task.await;

    let fresh = Arc::new(Counter::new("count", true, false));
    let fresh_calls = fresh.calls.clone();
    let agent = h.agent(
        Arc::new(Script::new(replies)),
        vec![fresh],
        Arc::new(AutoApprove),
    );
    agent.resume(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(fresh_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn denied_approval_skips_the_tool_and_tells_the_model() {
    let h = Harness::new();
    let tool = Arc::new(Counter::new("danger", false, true));
    let calls = tool.calls.clone();
    let script = Arc::new(Script::new(vec![
        Reply::Tool("danger", json!({})),
        Reply::Text("ok, skipped"),
    ]));
    let agent = h.agent(script.clone(), vec![tool], Arc::new(AutoDeny));
    let run = h.start(&agent, "go");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(out.final_text, "ok, skipped");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.kinds(&run),
        ["llm:completed", "approval:completed", "llm:completed"]
    );
    let (content, is_error) = last_tool_result(&script.seen.lock().unwrap()[1]).unwrap();
    assert!(is_error && content.contains("denied"), "{content}");
}

#[tokio::test]
async fn always_allow_is_remembered_and_journaled() {
    struct Always(AtomicU32);
    #[async_trait]
    impl ApprovalHandler for Always {
        async fn ask(&self, _: &ApprovalRequest) -> Decision {
            self.0.fetch_add(1, Ordering::SeqCst);
            Decision::AlwaysAllow
        }
    }
    let h = Harness::new();
    let tool = Arc::new(Counter::new("danger", false, true));
    let asks = Arc::new(Always(AtomicU32::new(0)));
    let replies = vec![
        Reply::Tool("danger", json!({})),
        Reply::Tool("danger", json!({"again": true})),
        Reply::Text("ok"),
    ];
    let agent = h.agent(
        Arc::new(Script::new(replies)),
        vec![tool.clone()],
        asks.clone(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        asks.0.load(Ordering::SeqCst),
        1,
        "second call is allowlisted"
    );
    assert_eq!(tool.calls.load(Ordering::SeqCst), 2);
    assert_eq!(h.store.allowlist("danger").unwrap(), ["*"]);
    assert_eq!(
        h.kinds(&run),
        [
            "llm:completed",
            "approval:completed",
            "tool:completed",
            "llm:completed",
            "approval:completed",
            "tool:completed",
            "llm:completed"
        ],
    );
}

#[tokio::test]
async fn transient_llm_errors_are_retried() {
    let h = Harness::new();
    let script = Arc::new(Script::new(vec![Reply::Text("finally")]));
    script.fail_first.store(2, Ordering::SeqCst);
    let agent = h.agent(script.clone(), vec![], Arc::new(AutoApprove));
    let mut events = agent.subscribe();
    let run = h.start(&agent, "go");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(out.final_text, "finally");
    assert_eq!(script.seen.lock().unwrap().len(), 3);
    let mut retries = 0;
    while let Ok(ev) = events.try_recv() {
        retries += matches!(ev, AgentEvent::Retrying { .. }) as u32;
    }
    assert_eq!(retries, 2);
}

#[tokio::test]
async fn gives_up_after_max_attempts() {
    let h = Harness::new();
    let script = Arc::new(Script::new(vec![Reply::Text("never")]));
    script.fail_first.store(99, Ordering::SeqCst);
    let agent = h.agent(script.clone(), vec![], Arc::new(AutoApprove));
    let run = h.start(&agent, "go");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(out.status, RunStatus::Failed);
    assert_eq!(script.seen.lock().unwrap().len(), MAX_LLM_ATTEMPTS as usize);
}

#[tokio::test]
async fn cancellation_interrupts_a_streaming_call() {
    let h = Harness::new();
    let mut script = Script::new(vec![Reply::Text("slow")]);
    script.delay = Duration::from_secs(30);
    let agent = h.agent(Arc::new(script), vec![], Arc::new(AutoApprove));
    let run = h.start(&agent, "go");
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c.cancel();
    });
    let out = agent.drive(&run, cancel).await.unwrap();
    assert_eq!(out.status, RunStatus::Interrupted);
    assert_eq!(h.store.run(&run).unwrap().unwrap().status, "interrupted");
    assert_eq!(
        h.store.session_messages("s1").unwrap().len(),
        1,
        "partial text is discarded"
    );
}

#[tokio::test]
async fn stops_at_max_iterations() {
    let h = Harness::new();
    let tool = Arc::new(Counter::new("count", true, false));
    let replies = (0..20).map(|_| Reply::Tool("count", json!({}))).collect();
    let agent = h.agent(
        Arc::new(Script::new(replies)),
        vec![tool.clone()],
        Arc::new(AutoApprove),
    );
    let run = h.start(&agent, "loop forever");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(out.status, RunStatus::Completed);
    assert!(out.final_text.contains("max_iterations"));
    assert_eq!(tool.calls.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn completed_runs_are_not_resumable() {
    let h = Harness::new();
    let agent = h.agent(
        Arc::new(Script::new(vec![Reply::Text("x")])),
        vec![],
        Arc::new(AutoApprove),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert!(matches!(
        agent.resume(&run, CancellationToken::new()).await,
        Err(AgentError::NotResumable(..))
    ));
}

fn eight_tool_turns() -> Vec<Reply> {
    let mut r: Vec<Reply> = (0..8).map(|_| Reply::Tool("count", json!({}))).collect();
    r.push(Reply::Text("finished"));
    r
}

/// Small enough that a few turns exceed 75% of it.
const TINY_WINDOW: u64 = 40;

#[tokio::test]
async fn compaction_summarizes_old_context_and_keeps_going() {
    let h = Harness::new();
    let tool = Arc::new(Counter::new("count", true, false));
    let script = Arc::new(Script::new(eight_tool_turns()));
    let agent = h.agent_with_window(
        script.clone(),
        vec![tool.clone()],
        Arc::new(AutoApprove),
        TINY_WINDOW,
    );
    let mut events = agent.subscribe();
    let run = h.start(&agent, "go");
    let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        (out.status, out.final_text.as_str()),
        (RunStatus::Completed, "finished")
    );
    assert_eq!(tool.calls.load(Ordering::SeqCst), 8);
    let kinds = h.kinds(&run);
    let compactions = kinds
        .iter()
        .filter(|k| *k == "compaction:completed")
        .count();
    assert!(compactions >= 1, "{kinds:?}");
    assert_eq!(compactions as u32, script.summaries.load(Ordering::SeqCst));

    let seen = script.seen.lock().unwrap();
    let last = seen.iter().rev().find(|r| r.system == "system").unwrap();
    assert!(
        last.messages[0]
            .text()
            .starts_with(crate::compaction::SUMMARY_PREFIX),
        "{:?}",
        last.messages[0]
    );
    // Every tool result in the request has its tool call in the request too.
    let uses: Vec<&str> = last
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for b in last.messages.iter().flat_map(|m| m.content.iter()) {
        if let ContentBlock::ToolResult { tool_use_id, .. } = b {
            assert!(
                uses.contains(&tool_use_id.as_str()),
                "orphaned result {tool_use_id}"
            );
        }
    }
    drop(seen);

    let mut saw = false;
    while let Ok(ev) = events.try_recv() {
        saw |= matches!(ev, AgentEvent::Compacted { .. });
        if let AgentEvent::TextDelta { text, .. } = &ev {
            assert!(
                !text.contains("summary"),
                "summaries must not stream as assistant text"
            );
        }
    }
    assert!(saw);
}

#[tokio::test]
async fn compaction_replays_after_resume() {
    struct FailAt(Arc<Script>, AtomicU32, u32);
    #[async_trait]
    impl Provider for FailAt {
        fn id(&self) -> &str {
            "f"
        }
        async fn stream(
            &self,
            r: ChatRequest,
            c: CancellationToken,
        ) -> Result<EventStream, ProviderError> {
            if r.system == "system" && self.1.fetch_add(1, Ordering::SeqCst) + 1 == self.2 {
                return Err(ProviderError::Http {
                    status: 400,
                    body: "bad".into(),
                    retry_after: None,
                });
            }
            self.0.stream(r, c).await
        }
    }
    let h = Harness::new();
    let tool = Arc::new(Counter::new("count", true, false));
    // The first attempt fails permanently on its 7th agent model call.
    let first = Arc::new(Script::new(eight_tool_turns()));
    let fp: Arc<dyn Provider> = Arc::new(FailAt(first.clone(), AtomicU32::new(0), 7));
    let mut reg = ToolRegistry::new();
    reg.register(tool.clone());
    let a = Agent::new(
        Journal::new(h.store.clone()),
        Arc::new(move |_| Ok(fp.clone())),
        Arc::new(reg),
        Arc::new(AutoApprove),
        AgentConfig {
            retry_base: Duration::from_millis(1),
            default_context_window: TINY_WINDOW,
            ..AgentConfig::default()
        },
    );
    let run = h.start(&a, "go");
    assert_eq!(
        a.drive(&run, CancellationToken::new())
            .await
            .unwrap()
            .status,
        RunStatus::Failed
    );
    let summaries_before = first.summaries.load(Ordering::SeqCst);
    assert!(summaries_before >= 1, "{:?}", h.kinds(&run));
    assert_eq!(tool.calls.load(Ordering::SeqCst), 6);

    let second = Arc::new(Script::new(eight_tool_turns()));
    let agent = h.agent_with_window(
        second.clone(),
        vec![tool.clone()],
        Arc::new(AutoApprove),
        TINY_WINDOW,
    );
    let out = agent.resume(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        (out.status, out.final_text.as_str()),
        (RunStatus::Completed, "finished")
    );
    assert_eq!(
        tool.calls.load(Ordering::SeqCst),
        8,
        "replayed tools did not re-run"
    );
    // Earlier compactions replayed from the journal; only later ones ran.
    let total = h
        .kinds(&run)
        .iter()
        .filter(|k| *k == "compaction:completed")
        .count() as u32;
    assert_eq!(
        summaries_before + second.summaries.load(Ordering::SeqCst),
        total
    );
}

#[test]
fn triage_resumes_recent_and_interrupts_stale_runs() {
    let h = Harness::new();
    let agent = h.agent(Arc::new(Script::new(vec![])), vec![], Arc::new(AutoApprove));
    let recent = h.start(&agent, "recent");
    let stale = h.start(&agent, "stale");
    let done = h.start(&agent, "done");
    h.store.set_run_status(&done, "completed", None).unwrap();
    let old = tern_store::now_ms() - 25 * 60 * 60 * 1000;
    h.store
        .conn()
        .unwrap()
        .execute(
            "UPDATE runs SET updated_at = ?1 WHERE id = ?2",
            (old, stale.clone()),
        )
        .unwrap();

    let resumable = triage_running_runs(&h.store, RESUME_MAX_AGE).unwrap();
    assert_eq!(
        resumable.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        [recent.as_str()]
    );
    assert_eq!(h.store.run(&stale).unwrap().unwrap().status, "interrupted");
    assert_eq!(h.store.run(&done).unwrap().unwrap().status, "completed");
}

#[test]
fn backoff_grows_and_caps() {
    let base = Duration::from_secs(1);
    assert!(backoff(base, 1) <= Duration::from_secs(1));
    assert!(backoff(base, 3) >= Duration::from_secs(2));
    assert!(backoff(base, 20) <= Duration::from_secs(30));
}

#[test]
fn assembler_keeps_block_order_and_tolerates_bad_json() {
    let mut a = Assembler::default();
    a.push(StreamEvent::TextDelta("a".into()));
    a.push(StreamEvent::ToolUseStart {
        id: "1".into(),
        name: "x".into(),
    });
    a.push(StreamEvent::ToolUseInputDelta {
        id: "1".into(),
        partial_json: "{not json".into(),
    });
    a.push(StreamEvent::ToolUseStart {
        id: "2".into(),
        name: "y".into(),
    });
    a.push(StreamEvent::TextDelta("b".into()));
    a.push(StreamEvent::MessageStop {
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    });
    let t = a.finish().unwrap();
    assert!(matches!(&t.message.content[0], ContentBlock::Text { text } if text == "a"));
    assert!(
        matches!(&t.message.content[1], ContentBlock::ToolUse { input, .. } if input["_invalid_arguments"] == "{not json")
    );
    assert!(
        matches!(&t.message.content[2], ContentBlock::ToolUse { input, .. } if *input == json!({}))
    );
    assert!(matches!(&t.message.content[3], ContentBlock::Text { text } if text == "b"));

    let incomplete = Assembler::default();
    assert!(incomplete.finish().unwrap_err().is_retryable());
}
