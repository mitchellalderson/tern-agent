//! Scripts that ask, write and run calls together (docs/python-tool-spec.md
//! §3.4, §3.5, PY1b and PY2), through the real loop with a stand-in for the
//! `python` tool that makes the calls a script would.

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;

use super::*;
use crate::script::{HostReply, ScriptLimits};

/// A `python` tool whose "script" is a list of calls: from its `calls`
/// argument, or (without one) the next plan queued with `plan`. With
/// `batch` they go out together, as `gather` would send them.
struct Driver {
    plans: Mutex<VecDeque<Vec<(&'static str, Value)>>>,
}

impl Driver {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            plans: Mutex::new(VecDeque::new()),
        })
    }
    fn plan(&self, calls: Vec<(&'static str, Value)>) {
        self.plans.lock().unwrap().push_back(calls);
    }
}

#[async_trait]
impl Tool for Driver {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: crate::script::PYTHON.into(),
            description: "py".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }
    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let host = crate::script::current_host().expect("the loop serves scripts");
        let calls: Vec<(String, Value)> = match args["calls"].as_array() {
            Some(list) => list
                .iter()
                .map(|c| (c["name"].as_str().unwrap().to_owned(), c["input"].clone()))
                .collect(),
            None => self
                .plans
                .lock()
                .unwrap()
                .pop_front()
                .expect("a plan")
                .into_iter()
                .map(|(n, i)| (n.to_owned(), i))
                .collect(),
        };
        let replies = if args["batch"].as_bool().unwrap_or(false) {
            host.calls(calls).await
        } else {
            let mut out = vec![];
            for (name, input) in calls {
                let r = host.call(&name, input).await;
                let abort = matches!(r, HostReply::Abort(_));
                out.push(r);
                if abort {
                    break;
                }
            }
            out
        };
        let lines: Vec<String> = replies
            .into_iter()
            .map(|r| match r {
                HostReply::Done(r) => r.content,
                HostReply::Refused(why) => format!("refused: {why}"),
                HostReply::Abort(why) => format!("aborted: {why}"),
            })
            .collect();
        ToolResult::ok(lines.join("\n"))
    }
}

fn agent_limited(
    h: &Harness,
    script: Arc<Script>,
    tools: Vec<Arc<dyn Tool>>,
    approvals: Arc<dyn ApprovalHandler>,
    limits: ScriptLimits,
) -> Agent {
    let mut reg = ToolRegistry::new();
    for t in tools {
        reg.register(t);
    }
    let p: Arc<dyn Provider> = script;
    Agent::new(
        Journal::new(h.store.clone()),
        Arc::new(move |_| Ok(p.clone())),
        Arc::new(reg),
        approvals,
        AgentConfig {
            retry_base: Duration::from_millis(1),
            approval_wait: Duration::from_millis(50),
            script: limits,
            ..AgentConfig::default()
        },
    )
}

fn writable() -> ScriptLimits {
    ScriptLimits {
        allow_writes: true,
        ..ScriptLimits::default()
    }
}

fn python_turn(calls: Value, batch: bool) -> Reply {
    Reply::Tool(
        "python",
        json!({ "code": format!("# script making {calls}"), "calls": calls, "batch": batch }),
    )
}

fn call(name: &str, input: Value) -> Value {
    json!({ "name": name, "input": input })
}

/// What the model was told by the python call (the last tool result).
fn said(script: &Script) -> String {
    let seen = script.seen.lock().unwrap();
    last_tool_result(seen.last().unwrap()).unwrap().0
}

/// What the `python` step returned, as journaled (the last drive's, since a
/// replayed step doesn't reach the model again).
fn python_output(h: &Harness, run: &str) -> String {
    let steps = h.store.steps(run).unwrap();
    let tool = steps
        .iter()
        .find(|s| s.kind == "tool" && s.input_json.contains("\"python\""))
        .expect("the python step");
    let r: ToolResult = serde_json::from_str(tool.output_json.as_deref().unwrap()).unwrap();
    r.content
}

/// A crash after a script's calls but before the script finished: the
/// `python` step is `started` again, its sub-steps completed.
fn crash_python_step(h: &Harness, run: &str) {
    let steps = h.store.steps(run).unwrap();
    let tool = steps
        .iter()
        .find(|s| s.kind == "tool" && s.input_json.contains("\"calls\""))
        .expect("the python step");
    h.store
        .start_step(run, tool.idx, "tool", &tool.input_hash, &tool.input_json)
        .unwrap();
    h.store.set_run_status(run, "running", None).unwrap();
}

fn py_substeps(h: &Harness, run: &str) -> usize {
    h.kinds(run)
        .iter()
        .filter(|k| *k == "py_call:completed")
        .count()
}

#[tokio::test]
async fn a_script_read_that_asks_waits_for_the_user_and_shows_the_script() {
    let h = Harness::new();
    let fetch = Arc::new(Fetch(AtomicU32::new(0)));
    let answer = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        // The first fetch taints the run; the script's second host is new.
        Reply::Tool("fetch", json!({ "host": "a.example" })),
        python_turn(
            json!([call("fetch", json!({ "host": "b.example" }))]),
            false,
        ),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![fetch.clone(), Driver::new()],
        answer.clone(),
        ScriptLimits::default(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = answer.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].egress_host.as_deref(), Some("b.example"));
    assert_eq!(
        asked[0].script_note(),
        Some("from a python script (call 1 of this script)")
    );
    assert!(
        asked[0].script_text().unwrap().contains("b.example"),
        "{:?}",
        asked[0].script
    );
    assert!(asked[0].headline().contains("from a python script"));
    assert_eq!(fetch.0.load(Ordering::SeqCst), 2);
    assert_eq!(said(&script), "page");
    // The ask is its own sub-step, then the call.
    assert_eq!(py_substeps(&h, &run), 2, "ask, call: {:?}", h.kinds(&run));
}

#[tokio::test]
async fn a_denied_script_call_raises_for_the_script_and_a_replay_doesnt_ask_again() {
    let h = Harness::new();
    let fetch = Arc::new(Fetch(AtomicU32::new(0)));
    let answer = Answer::new(Decision::Deny);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("fetch", json!({ "host": "a.example" })),
        python_turn(
            json!([
                call("fetch", json!({ "host": "evil.example" })),
                call("fetch", json!({ "host": "a.example" }))
            ]),
            false,
        ),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![fetch.clone(), Driver::new()],
        answer.clone(),
        ScriptLimits::default(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(answer.asked().len(), 1);
    assert!(
        said(&script).starts_with("refused: denied: the user did not approve"),
        "{}",
        said(&script)
    );
    assert!(said(&script).ends_with("page"), "the next call still ran");
    assert_eq!(fetch.0.load(Ordering::SeqCst), 2, "a, then a; never evil");
    crash_python_step(&h, &run);
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(answer.asked().len(), 1, "the denial was journaled");
    assert_eq!(fetch.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn at_most_max_asks_prompts_per_script() {
    let h = Harness::new();
    let send = sender();
    let answer = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        python_turn(
            json!([
                call("send", json!({ "to": "a" })),
                call("send", json!({ "to": "b" })),
                call("send", json!({ "to": "c" }))
            ]),
            false,
        ),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![send.clone(), Driver::new()],
        answer.clone(),
        ScriptLimits {
            max_asks: 2,
            ..writable()
        },
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(answer.asked().len(), 2);
    assert_eq!(send.calls.load(Ordering::SeqCst), 2);
    assert!(
        said(&script).contains("refused: too many approvals in one script (the limit is 2)"),
        "{}",
        said(&script)
    );
}

#[tokio::test]
async fn scripts_may_not_write_unless_the_config_allows_it() {
    let h = Harness::new();
    let send = sender();
    let answer = Answer::new(Decision::AllowOnce);
    let replies = || {
        Arc::new(Script::new(vec![
            python_turn(json!([call("send", json!({ "to": "a" }))]), false),
            Reply::Text("done"),
        ]))
    };
    let script = replies();
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![send.clone(), Driver::new()],
        answer.clone(),
        ScriptLimits::default(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert!(said(&script).contains("allow_writes"), "{}", said(&script));
    assert!(answer.asked().is_empty());
    assert_eq!(send.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn destructive_tools_are_never_callable_from_a_script() {
    let h = Harness::new();
    let wipe = Arc::new(Counter::with_policy(
        "wipe",
        ToolPolicy::new(Effect::Destructive, false),
    ));
    let answer = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        python_turn(json!([call("wipe", json!({}))]), false),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![wipe.clone(), Driver::new()],
        answer.clone(),
        writable(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert!(said(&script).contains("destructive"), "{}", said(&script));
    assert!(answer.asked().is_empty());
    assert_eq!(wipe.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_approved_script_write_runs_once_is_ledgered_and_survives_a_crash() {
    let h = Harness::new();
    let send = sender();
    let answer = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        python_turn(json!([call("send", json!({ "to": "bob" }))]), false),
        Reply::Text("sent"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![send.clone(), Driver::new()],
        answer.clone(),
        writable(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(send.calls.load(Ordering::SeqCst), 1);
    let asked = answer.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].effect, Effect::Write);
    assert!(asked[0].script_note().is_some());
    assert_eq!(said(&script), "count=1");
    // The decision and the effect are in the ledger, as for a direct call.
    assert_eq!(ledger_kinds(&h), ["approval", "effect"]);

    crash_python_step(&h, &run);
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(send.calls.load(Ordering::SeqCst), 1, "never sent twice");
    assert_eq!(answer.asked().len(), 1, "never asked twice");
    assert_eq!(ledger_kinds(&h), ["approval", "effect"]);
}

#[tokio::test]
async fn a_granted_write_runs_without_asking_and_counts_toward_max_writes() {
    let h = Harness::new();
    let save = Arc::new(Counter::with_policy(
        "save",
        ToolPolicy::granted_write(false),
    ));
    let answer = Answer::new(Decision::Deny);
    let script = Arc::new(Script::new(vec![
        python_turn(
            json!([
                call("save", json!({})),
                call("save", json!({})),
                call("save", json!({}))
            ]),
            false,
        ),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![save.clone(), Driver::new()],
        answer.clone(),
        ScriptLimits {
            max_writes: 2,
            ..writable()
        },
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert!(answer.asked().is_empty());
    assert_eq!(save.calls.load(Ordering::SeqCst), 2);
    assert!(
        said(&script).contains("refused: too many writes in one script (the limit is 2)"),
        "{}",
        said(&script)
    );
}

#[tokio::test]
async fn a_readonly_run_refuses_script_writes() {
    let h = Harness::new();
    let send = sender();
    let answer = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        python_turn(json!([call("send", json!({}))]), false),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![send.clone(), Driver::new()],
        answer.clone(),
        writable(),
    );
    let run = h.start(&agent, "go");
    h.store.set_run_autonomy(&run, "readonly").unwrap();
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert!(said(&script).contains("read-only"), "{}", said(&script));
    assert!(answer.asked().is_empty());
    assert_eq!(send.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_tainted_scripts_write_goes_to_the_outbox_once_and_says_queued() {
    for hang in [false, true] {
        let h = Harness::new();
        let send = sender();
        let fetch = Arc::new(Fetch(AtomicU32::new(0)));
        let nobody = Nobody::new(hang);
        let script = Arc::new(Script::new(vec![
            // Reading a page taints the run: the write must ask.
            Reply::Tool("fetch", json!({ "host": "news.example" })),
            python_turn(json!([call("send", json!({ "to": "bob" }))]), false),
            Reply::Text("It's queued for you."),
        ]));
        let agent = agent_limited(
            &h,
            script.clone(),
            vec![send.clone(), fetch.clone(), Driver::new()],
            nobody.clone(),
            writable(),
        );
        let run = h.start(&agent, "go");
        let out = agent.drive(&run, CancellationToken::new()).await.unwrap();
        assert_eq!(out.status, RunStatus::Completed, "hang={hang}");
        assert_eq!(send.calls.load(Ordering::SeqCst), 0, "nothing sent");
        let items = h.store.pending_outbox_for_run(&run).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tool, "send");
        assert_eq!(items[0].input_json, json!({ "to": "bob" }).to_string());
        assert!(items[0].taint.is_some());
        let result: Value = serde_json::from_str(&said(&script)).unwrap();
        assert_eq!(
            result,
            json!({ "queued": true, "outbox": items[0].id, "tool": "send" })
        );
        assert_eq!(nobody.asked.load(Ordering::SeqCst), 1);

        // A re-run of the script answers from the journal: no second
        // prompt, no second outbox item, the same `queued` value.
        crash_python_step(&h, &run);
        agent.drive(&run, CancellationToken::new()).await.unwrap();
        assert_eq!(nobody.asked.load(Ordering::SeqCst), 1);
        assert_eq!(h.store.outbox(None, 10).unwrap().len(), 1);
        assert_eq!(
            serde_json::from_str::<Value>(&python_output(&h, &run)).unwrap(),
            result,
            "the same `queued` value on replay"
        );

        // Approving it later runs the one call, through the outbox.
        let exec = approve(&h, items[0].id, None);
        let run_row = h.store.run(&exec).unwrap().unwrap();
        assert_eq!(run_row.trigger, crate::outbox::TRIGGER);
        agent.drive(&exec, CancellationToken::new()).await.unwrap();
        assert_eq!(send.calls.load(Ordering::SeqCst), 1);
    }
}

/// A read that takes a while and counts how many run at once.
struct Slow {
    now: Arc<AtomicUsize>,
    most: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "slow".into(),
            description: "slow".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        Effect::Read
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }
    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(n, Ordering::SeqCst);
        // Later calls finish first, so completion order isn't call order.
        let i = args["i"].as_u64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(60 - (i * 5).min(50))).await;
        self.now.fetch_sub(1, Ordering::SeqCst);
        ToolResult::ok(format!("r{i}"))
    }
}

#[tokio::test]
async fn gathered_calls_run_in_parallel_up_to_the_limit_and_answer_in_order() {
    let h = Harness::new();
    let slow = Slow {
        now: Arc::new(AtomicUsize::new(0)),
        most: Arc::new(AtomicUsize::new(0)),
    };
    let most = slow.most.clone();
    let calls: Vec<Value> = (0..8).map(|i| call("slow", json!({ "i": i }))).collect();
    let script = Arc::new(Script::new(vec![
        python_turn(json!(calls), true),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![Arc::new(slow), Driver::new()],
        Answer::new(Decision::Deny),
        ScriptLimits {
            max_parallel: 3,
            ..ScriptLimits::default()
        },
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(most.load(Ordering::SeqCst), 3, "three at a time");
    assert_eq!(
        said(&script),
        "r0\nr1\nr2\nr3\nr4\nr5\nr6\nr7",
        "answers in the order asked"
    );
    // One sub-step per call, numbered in call order.
    assert_eq!(py_substeps(&h, &run), 8);

    // A crash mid-script re-runs from the journal: nothing runs again.
    let before = most.load(Ordering::SeqCst);
    crash_python_step(&h, &run);
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(python_output(&h, &run), "r0\nr1\nr2\nr3\nr4\nr5\nr6\nr7");
    assert_eq!(most.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn one_approval_in_a_gathered_batch_covers_the_same_host() {
    let h = Harness::new();
    let fetch = Arc::new(Fetch(AtomicU32::new(0)));
    let answer = Answer::new(Decision::AllowOnce);
    let calls: Vec<Value> = (0..4)
        .map(|_| call("fetch", json!({ "host": "new.example" })))
        .collect();
    let script = Arc::new(Script::new(vec![
        Reply::Tool("fetch", json!({ "host": "a.example" })),
        python_turn(json!(calls), true),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script,
        vec![fetch.clone(), Driver::new()],
        answer.clone(),
        ScriptLimits::default(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(answer.asked().len(), 1, "one prompt for four fetches");
    assert_eq!(fetch.0.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn a_standing_host_grant_answers_a_script_without_a_prompt() {
    let h = Harness::new();
    let fetch = Arc::new(Fetch(AtomicU32::new(0)));
    h.store
        .add_allowlist("fetch", &approval::host_pattern("granted.example"))
        .unwrap();
    let answer = Answer::new(Decision::Deny);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("fetch", json!({ "host": "a.example" })),
        python_turn(
            json!([
                call("fetch", json!({ "host": "granted.example" })),
                call("fetch", json!({ "host": "granted.example" }))
            ]),
            true,
        ),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script,
        vec![fetch.clone(), Driver::new()],
        answer.clone(),
        ScriptLimits::default(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert!(answer.asked().is_empty());
    assert_eq!(fetch.0.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_script_that_changes_on_resume_after_a_write_ran_fails_safe() {
    let h = Harness::new();
    let send = sender();
    let answer = Answer::new(Decision::AllowOnce);
    let driver = Driver::new();
    driver.plan(vec![
        ("send", json!({ "to": "a" })),
        ("send", json!({ "to": "b" })),
    ]);
    // After the crash the script asks for something else second.
    driver.plan(vec![
        ("send", json!({ "to": "a" })),
        ("send", json!({ "to": "c" })),
    ]);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("python", json!({ "go": 1 })),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![send.clone(), driver],
        answer.clone(),
        writable(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(send.calls.load(Ordering::SeqCst), 2);

    // The python step's input names no `calls`, so reset it by hand.
    let steps = h.store.steps(&run).unwrap();
    let tool = steps.iter().find(|s| s.kind == "tool").unwrap();
    h.store
        .start_step(&run, tool.idx, "tool", &tool.input_hash, &tool.input_json)
        .unwrap();
    h.store.set_run_status(&run, "running", None).unwrap();
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        send.calls.load(Ordering::SeqCst),
        2,
        "c was never sent, a and b never repeated"
    );
    let said = python_output(&h, &run);
    assert!(
        said.contains("aborted: script changed on resume after these calls ran:"),
        "{said}"
    );
    assert!(said.contains("send"), "{said}");
}

#[tokio::test]
async fn a_script_that_changes_on_resume_after_only_reads_carries_on_live() {
    let h = Harness::new();
    let fetch = Arc::new(Fetch(AtomicU32::new(0)));
    let driver = Driver::new();
    driver.plan(vec![
        ("fetch", json!({ "host": "a.example" })),
        ("fetch", json!({ "host": "b.example" })),
    ]);
    driver.plan(vec![
        ("fetch", json!({ "host": "a.example" })),
        ("fetch", json!({ "host": "c.example" })),
    ]);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("python", json!({ "go": 1 })),
        Reply::Text("done"),
    ]));
    let agent = agent_limited(
        &h,
        script.clone(),
        vec![fetch.clone(), driver],
        Answer::new(Decision::AllowOnce),
        ScriptLimits::default(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(fetch.0.load(Ordering::SeqCst), 2);
    let steps = h.store.steps(&run).unwrap();
    let tool = steps.iter().find(|s| s.kind == "tool").unwrap();
    h.store
        .start_step(&run, tool.idx, "tool", &tool.input_hash, &tool.input_json)
        .unwrap();
    h.store.set_run_status(&run, "running", None).unwrap();
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    // a replayed from the journal; c ran live (repeating a read is safe).
    assert_eq!(fetch.0.load(Ordering::SeqCst), 3);
    assert_eq!(python_output(&h, &run), "page\npage");
}

#[tokio::test]
async fn a_cancelled_approval_wait_leaves_the_script_to_ask_again_on_resume() {
    let h = Harness::new();
    let fetch = Arc::new(Fetch(AtomicU32::new(0)));
    let nobody = Nobody::new(true);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("fetch", json!({ "host": "a.example" })),
        python_turn(
            json!([call("fetch", json!({ "host": "b.example" }))]),
            false,
        ),
        Reply::Text("done"),
    ]));
    let agent = Arc::new(agent_limited(
        &h,
        script,
        vec![fetch.clone(), Driver::new()],
        nobody.clone(),
        ScriptLimits::default(),
    ));
    let run = h.start(&agent, "go");
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let watcher = {
        let nobody = nobody.clone();
        tokio::spawn(async move {
            while nobody.asked.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            stop.cancel();
        })
    };
    let out = agent.drive(&run, cancel).await.unwrap();
    watcher.await.unwrap();
    assert_eq!(out.status, RunStatus::Interrupted);
    assert_eq!(fetch.0.load(Ordering::SeqCst), 1, "b was never reached");
}
