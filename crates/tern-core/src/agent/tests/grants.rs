//! Scoped grants: "allow for this run / conversation", "always allow this",
//! keyed on exactly what the user was shown ([`Tool::grant_key`]).

use super::*;
use crate::approval::GrantOffer;
use crate::tool::GrantKey;

/// A read that always asks (like `connector_test` on a draft that reaches
/// the network), with a grant key naming its `target` argument.
struct Keyed {
    name: &'static str,
    effect: Effect,
    always: bool,
    calls: AtomicU32,
}

impl Keyed {
    fn new(name: &'static str, effect: Effect) -> Arc<Self> {
        Arc::new(Self {
            name,
            effect,
            always: false,
            calls: AtomicU32::new(0),
        })
    }

    /// Like `new`, but also allows an "always" command pattern.
    fn with_pattern(name: &'static str, effect: Effect) -> Arc<Self> {
        Arc::new(Self {
            name,
            effect,
            always: true,
            calls: AtomicU32::new(0),
        })
    }
}

/// A run tainted from the start.
fn tainted_run(h: &Harness) -> String {
    let model: ModelRef = "script:m".parse().unwrap();
    let input = Journal::new(h.store.clone())
        .to_row(&Message::user_text("from a stranger"))
        .unwrap();
    h.store
        .create_run(
            &tern_store::NewRun {
                id: "tainted",
                session_id: &h.session,
                trigger: "telegram",
                model: &model.to_string(),
                system_prompt: "system",
                taint: Some("message from a non-paired sender"),
                ..tern_store::NewRun::default()
            },
            &input,
        )
        .unwrap()
        .id
}

#[tokio::test]
async fn a_tainted_run_offers_no_grant_even_for_a_taint_contained_tool() {
    // `channel_post` keeps its grants in a tainted run, so `check` asks as
    // if clean; the grant offer must still test the run's taint.
    let h = Harness::new();
    let tool = Keyed::new("channel_post", Effect::Write);
    let asks = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        call("channel_post", "a", 1),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool], asks.clone());
    let run = tainted_run(&h);
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].grant.is_none() && asked[0].taint.is_some(),
        "{asked:?}"
    );
}

#[tokio::test]
async fn always_with_both_a_pattern_and_a_grant_stores_the_key_only() {
    let h = Harness::new();
    let tool = Keyed::with_pattern("shell", Effect::Write);
    let asks = Answer::new(Decision::AlwaysAllow);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("shell", json!({"command": "echo hi", "target": "a"})),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    // One meaning of "always": the prompt offers the key, not the pattern.
    assert!(
        asked[0].pattern.is_none() && asked[0].grant.is_some(),
        "{asked:?}"
    );
    assert_eq!(
        h.store.allowlist("shell").unwrap(),
        ["key:target a#abc".to_owned()]
    );
}

#[tokio::test]
async fn a_call_covered_by_a_grant_runs_and_the_ledger_says_so() {
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Read);
    let asks = Answer::new(Decision::CoveredByGrant);
    let script = Arc::new(Script::new(vec![call("keyed", "a", 1), Reply::Text("ok")]));
    let agent = h.agent(script, vec![tool.clone()], asks);
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    let rows = h
        .store
        .ledger(&tern_store::LedgerFilter {
            kind: Some("approval".into()),
            ..Default::default()
        })
        .unwrap();
    let e = &rows[0].entry;
    assert!(
        e.summary.starts_with("covered by a grant: "),
        "{}",
        e.summary
    );
    assert_eq!(e.detail.as_ref().unwrap()["decision"], "covered by a grant");
    assert_eq!(e.detail.as_ref().unwrap()["grant"]["key"], "target a#abc");
    // The journaled outcome says so too, for clients.
    let step = &h.store.steps(&run).unwrap()[1];
    assert_eq!(step.kind, "approval");
    assert_eq!(step.output_json.as_deref(), Some("\"approved_by_grant\""));
}

#[async_trait]
impl Tool for Keyed {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "keyed".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        self.effect
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(self.effect, false).forced(true)
    }
    fn allow_always(&self) -> bool {
        self.always
    }
    fn grant_key(&self, args: &Value) -> Option<GrantKey> {
        let t = args["target"].as_str()?;
        Some(GrantKey {
            key: format!("target {t}#abc"),
            label: format!("target {t}"),
            ..GrantKey::default()
        })
    }
    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("done")
    }
}

fn call(tool: &'static str, target: &str, n: u32) -> Reply {
    Reply::Tool(tool, json!({ "target": target, "n": n }))
}

#[tokio::test]
async fn a_run_grant_covers_only_the_same_key_in_that_run() {
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Read);
    let asks = Answer::new(Decision::AllowRun);
    let script = Arc::new(Script::new(vec![
        call("keyed", "a", 1),
        call("keyed", "a", 2),
        call("keyed", "a", 3),
        call("keyed", "b", 4),
        Reply::Text("ok"),
        call("keyed", "a", 5),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    // One ask for "a" (the rest are covered), one for the different "b".
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(
        asked[0].grant,
        Some(GrantOffer {
            key: "target a#abc".into(),
            label: "target a".into()
        })
    );
    assert_eq!(asked[1].grant.as_ref().unwrap().label, "target b");
    assert_eq!(tool.calls.load(Ordering::SeqCst), 4);
    // Nothing standing was written.
    assert!(h.store.allowlist("keyed").unwrap().is_empty());

    // Another run in the same conversation is asked again.
    let run2 = h.start(&agent, "again");
    agent.drive(&run2, CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len(), 3);
}

#[tokio::test]
async fn a_conversation_grant_covers_later_runs_there_but_not_other_conversations() {
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Read);
    let asks = Answer::new(Decision::AllowSession);
    let script = Arc::new(Script::new(vec![
        call("keyed", "a", 1),
        Reply::Text("one"),
        call("keyed", "a", 2),
        Reply::Text("two"),
        call("keyed", "a", 3),
        Reply::Text("three"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let run2 = h.start(&agent, "again");
    agent.drive(&run2, CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len(), 1, "the second run is covered");
    let other = start_fresh(&h, &agent, "other-conversation");
    agent.drive(&other, CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len(), 2, "another conversation asks");
    assert_eq!(tool.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn always_allow_stores_a_key_grant_that_covers_nothing_else() {
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Read);
    let other_tool = Keyed::new("keyed2", Effect::Read);
    let asks = Answer::new(Decision::AlwaysAllow);
    let script = Arc::new(Script::new(vec![
        call("keyed", "a", 1),
        call("keyed", "a", 2),
        // Same key, different tool: a grant never crosses tools.
        call("keyed2", "a", 3),
        // Same tool, different target.
        call("keyed", "b", 4),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool.clone(), other_tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len(), 3);
    assert_eq!(h.store.allowlist("keyed").unwrap().len(), 2);
    assert_eq!(
        h.store.allowlist("keyed2").unwrap(),
        ["key:target a#abc".to_owned()]
    );
    assert!(
        h.store
            .allowlist("keyed")
            .unwrap()
            .contains(&"key:target a#abc".to_owned())
    );
    assert_eq!(
        tool.calls.load(Ordering::SeqCst) + other_tool.calls.load(Ordering::SeqCst),
        4
    );
}

#[tokio::test]
async fn a_destructive_call_offers_no_grant_and_asks_every_time() {
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Destructive);
    let asks = Answer::new(Decision::AllowRun);
    let script = Arc::new(Script::new(vec![
        call("keyed", "a", 1),
        call("keyed", "a", 2),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 2);
    assert!(asked.iter().all(|r| r.grant.is_none()));
}

#[tokio::test]
async fn a_tainted_run_offers_no_grant_and_a_grant_given_before_does_not_apply() {
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Read);
    // A standing grant exists, and the conversation has one too.
    h.store
        .add_allowlist("keyed", &crate::approval::key_pattern("target a#abc"))
        .unwrap();
    let asks = Answer::new(Decision::AllowRun);
    let script = Arc::new(Script::new(vec![
        call("keyed", "a", 1),
        call("keyed", "a", 2),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let model: ModelRef = "script:m".parse().unwrap();
    let input = Journal::new(h.store.clone())
        .to_row(&Message::user_text("from a stranger"))
        .unwrap();
    let run = h
        .store
        .create_run(
            &tern_store::NewRun {
                id: "tainted",
                session_id: &h.session,
                trigger: "telegram",
                model: &model.to_string(),
                system_prompt: "system",
                taint: Some("message from a non-paired sender"),
                ..tern_store::NewRun::default()
            },
            &input,
        )
        .unwrap();
    agent
        .drive(&run.id, CancellationToken::new())
        .await
        .unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 2, "grants don't cover a tainted run");
    assert!(asked.iter().all(|r| r.grant.is_none() && r.taint.is_some()));
}

#[tokio::test]
async fn an_unforbidden_untainted_grant_is_released_through_check_not_around_it() {
    // Autonomy readonly still refuses non-read effects even with a grant.
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Write);
    h.store
        .add_allowlist("keyed", &crate::approval::key_pattern("target a#abc"))
        .unwrap();
    let asks = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![call("keyed", "a", 1), Reply::Text("ok")]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    h.store.set_run_autonomy(&run, "readonly").unwrap();
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
    assert!(asks.asked().is_empty());
}

/// A stand-in for `shell` running a covered script: destructive, forced to
/// ask, with a grant key that carries a note and an audit record.
struct ScriptLike {
    calls: AtomicU32,
    /// How many times the host said a grant decided a call.
    applied: AtomicU32,
    /// `Some` when no grant may be offered, and why.
    unavailable: Option<&'static str>,
}

#[async_trait]
impl Tool for ScriptLike {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".into(),
            description: "shell".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        Effect::Destructive
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Destructive, false).forced(true)
    }
    fn grant_key(&self, args: &Value) -> Option<GrantKey> {
        let c = args["command"].as_str()?;
        self.unavailable.is_none().then(|| GrantKey {
            key: format!("python script {c}#h1"),
            label: format!("python3 {c} (version 3fa9c1d2)"),
            note: Some("Imports from other files aren't covered".into()),
            audit: Some(json!({"sha256": "3fa9c1d2", "sandbox": "verified: test"}).to_string()),
        })
    }
    fn grant_unavailable(&self, _: &Value) -> Option<String> {
        self.unavailable.map(Into::into)
    }
    fn grant_applied(&self, _: &str, _: &str) {
        self.applied.fetch_add(1, Ordering::SeqCst);
    }
    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("done")
    }
}

fn script_like(unavailable: Option<&'static str>) -> Arc<ScriptLike> {
    Arc::new(ScriptLike {
        calls: AtomicU32::new(0),
        applied: AtomicU32::new(0),
        unavailable,
    })
}

fn run_script(cmd: &str) -> Reply {
    Reply::Tool("shell", json!({ "command": cmd }))
}

#[tokio::test]
async fn a_shell_script_grant_is_offered_with_its_note_and_covers_only_that_command() {
    let h = Harness::new();
    let tool = script_like(None);
    let asks = Answer::new(Decision::AllowRun);
    let script = Arc::new(Script::new(vec![
        run_script("a.py x"),
        run_script("a.py x"),
        run_script("a.py y"),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 2, "the identical repeat is covered: {asked:?}");
    assert_eq!(asked[0].effect, Effect::Destructive);
    assert!(asked[0].grant.is_some());
    assert_eq!(
        asked[0].notice.as_deref(),
        Some("Imports from other files aren't covered")
    );
    assert_eq!(tool.calls.load(Ordering::SeqCst), 3);
    // The auto-run is in the ledger as approved by that grant.
    let rows = h
        .store
        .ledger(&tern_store::LedgerFilter {
            kind: Some("approval".into()),
            ..Default::default()
        })
        .unwrap();
    let used: Vec<_> = rows
        .iter()
        .filter(|r| r.entry.detail.as_ref().unwrap()["decision"] == "approved_by_grant")
        .collect();
    assert_eq!(used.len(), 1, "{rows:?}");
    let d = used[0].entry.detail.as_ref().unwrap();
    assert_eq!(d["grant"]["key"], "python script a.py x#h1");
    assert_eq!(d["audit"]["sha256"], "3fa9c1d2");
    assert_eq!(d["audit"]["sandbox"], "verified: test");
    assert_eq!(d["tainted"], false);
}

#[tokio::test]
async fn a_shell_script_grant_is_ignored_when_tainted_or_read_only() {
    // Tainted: it asks, no grant is offered, and a stored grant is ignored.
    let h = Harness::new();
    let tool = script_like(None);
    let asks = Answer::new(Decision::AllowOnce);
    h.store
        .add_allowlist("shell", &approval::key_pattern("python script a.py#h1"))
        .unwrap();
    let script = Arc::new(Script::new(vec![run_script("a.py"), Reply::Text("ok")]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = tainted_run(&h);
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].grant.is_none() && asked[0].notice.is_none(),
        "{asked:?}"
    );

    // Read-only: refused without asking, whatever grants exist.
    let h = Harness::new();
    let tool = script_like(None);
    let asks = Answer::new(Decision::AllowOnce);
    h.store
        .add_allowlist("shell", &approval::key_pattern("python script a.py#h1"))
        .unwrap();
    let script = Arc::new(Script::new(vec![run_script("a.py"), Reply::Text("ok")]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    h.store.set_run_autonomy(&run, "readonly").unwrap();
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert!(asks.asked().is_empty());
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn without_a_key_the_card_says_why_and_every_call_asks() {
    let h = Harness::new();
    let why = "Not auto-approved: the sandbox isn't confirmed active (test).";
    let tool = script_like(Some(why));
    let asks = Answer::new(Decision::AllowRun);
    let script = Arc::new(Script::new(vec![
        run_script("a.py"),
        run_script("a.py"),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    let asked = asks.asked();
    assert_eq!(asked.len(), 2, "no grant could be given, so both ask");
    for a in &asked {
        assert!(a.grant.is_none());
        assert_eq!(a.notice.as_deref(), Some(why));
    }
    assert_eq!(tool.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn yolo_runs_are_not_logged_as_decided_by_a_grant() {
    // Yolo allows before any grant is looked at, so a stored grant didn't
    // decide the call: no approved_by_grant entry (and no tainted: true one).
    let h = Harness::new();
    let tool = script_like(None);
    let asks = Answer::new(Decision::AllowOnce);
    h.store
        .add_allowlist("shell", &approval::key_pattern("python script a.py#h1"))
        .unwrap();
    let script = Arc::new(Script::new(vec![run_script("a.py"), Reply::Text("ok")]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = tainted_run(&h);
    h.store.set_run_autonomy(&run, "yolo").unwrap();
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert!(asks.asked().is_empty());
    let rows = h
        .store
        .ledger(&tern_store::LedgerFilter {
            kind: Some("approval".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        rows.iter()
            .all(|r| r.entry.detail.as_ref().unwrap()["decision"] != "approved_by_grant"),
        "{rows:?}"
    );
}

#[tokio::test]
async fn an_approval_replayed_for_a_call_that_never_ran_asks_again() {
    // Crash between the journaled approval and the tool step: what the call
    // would run may have changed, so the replayed approval isn't enough.
    let h = Harness::new();
    let tool = script_like(None);
    let asks = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![run_script("a.py"), Reply::Text("one")]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    let kinds = h.kinds(&run);
    assert_eq!(
        &kinds[..3],
        ["llm:completed", "approval:completed", "tool:completed"]
    );
    // The process died before the tool step was recorded.
    h.store.truncate_steps(&run, 2).unwrap();

    let script2 = Arc::new(Script::new(vec![Reply::Text("after")]));
    let agent2 = h.agent(script2, vec![tool.clone()], asks.clone());
    agent2.drive(&run, CancellationToken::new()).await.unwrap();
    // Not run again on the old answer, and the model is told why.
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    let steps = h.store.steps(&run).unwrap();
    assert_eq!(steps[1].kind, "approval");
    assert_eq!(steps[2].kind, "tool");
    assert!(
        steps[2]
            .output_json
            .as_deref()
            .unwrap()
            .contains("approved before tern restarted"),
        "{:?}",
        steps[2].output_json
    );
    assert_eq!(
        asks.asked().len(),
        1,
        "the replayed approval isn't asked again"
    );
}

#[tokio::test]
async fn a_replayed_approval_for_a_tool_without_plans_still_runs() {
    // Tools with no per-call key keep today's behaviour on resume.
    let h = Harness::new();
    let tool = Keyed::new("keyed", Effect::Read);
    let asks = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("keyed", json!({})),
        Reply::Text("one"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    h.store.truncate_steps(&run, 2).unwrap();
    let agent2 = h.agent(
        Arc::new(Script::new(vec![Reply::Text("after")])),
        vec![tool.clone()],
        asks.clone(),
    );
    agent2.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        tool.calls.load(Ordering::SeqCst),
        2,
        "ran on the replayed approval"
    );
}

#[tokio::test]
async fn the_tool_is_told_when_a_grant_decided_its_call_and_not_otherwise() {
    let h = Harness::new();
    let tool = script_like(None);
    let asks = Answer::new(Decision::AllowRun);
    let script = Arc::new(Script::new(vec![
        run_script("a.py"),
        run_script("a.py"),
        Reply::Text("ok"),
    ]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(asks.asked().len(), 1);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 2);
    // Only the second call was decided by the grant; the first was a human answer.
    assert_eq!(tool.applied.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_replayed_approval_is_refused_even_when_no_key_can_be_planned_now() {
    // After a restart the sandbox probe hasn't landed, so the call has no key
    // now (only a reason): the replayed approval still must not run it.
    let h = Harness::new();
    let tool = script_like(Some(
        "Not auto-approved: the sandbox isn't confirmed active (still checking).",
    ));
    let asks = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![run_script("a.py"), Reply::Text("one")]));
    let agent = h.agent(script, vec![tool.clone()], asks.clone());
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    h.store.truncate_steps(&run, 2).unwrap();
    let agent2 = h.agent(
        Arc::new(Script::new(vec![Reply::Text("after")])),
        vec![tool.clone()],
        asks.clone(),
    );
    agent2.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(
        tool.calls.load(Ordering::SeqCst),
        1,
        "not run on the old answer"
    );
    let steps = h.store.steps(&run).unwrap();
    assert!(
        steps[2]
            .output_json
            .as_deref()
            .unwrap()
            .contains("approved before tern restarted")
    );
}
