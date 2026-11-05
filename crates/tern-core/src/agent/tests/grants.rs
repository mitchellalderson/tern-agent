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
    calls: AtomicU32,
}

impl Keyed {
    fn new(name: &'static str, effect: Effect) -> Arc<Self> {
        Arc::new(Self {
            name,
            effect,
            calls: AtomicU32::new(0),
        })
    }
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
        false
    }
    fn grant_key(&self, args: &Value) -> Option<GrantKey> {
        let t = args["target"].as_str()?;
        Some(GrantKey {
            key: format!("target {t}#abc"),
            label: format!("target {t}"),
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
