//! P7: "Always allow" is learned, not on every card. After three "Allow
//! once" answers to exactly the same thing (agent, tool, target) in 14 days
//! the card offers it; never for destructive calls or routine runs.

use super::*;
use crate::tool::GrantKey;

/// A tool whose grant key names its `target` argument.
struct Target {
    effect: Effect,
}

#[async_trait]
impl Tool for Target {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "send".into(),
            description: "send".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        self.effect
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(self.effect, false).forced(true)
    }
    fn grant_key(&self, args: &Value) -> Option<GrantKey> {
        let t = args["target"].as_str()?;
        Some(GrantKey {
            key: format!("to {t}"),
            label: format!("to {t}"),
            ..GrantKey::default()
        })
    }
    async fn call(&self, _: &ToolCtx, _: Value) -> ToolResult {
        ToolResult::ok("sent")
    }
}

fn agent(h: &Harness, script: Arc<Script>, effect: Effect, a: Arc<dyn ApprovalHandler>) -> Agent {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(Target { effect }));
    let p: Arc<dyn Provider> = script;
    Agent::new(
        Journal::new(h.store.clone()),
        Arc::new(move |_| Ok(p.clone())),
        Arc::new(reg),
        a,
        AgentConfig {
            retry_base: Duration::from_millis(1),
            learn_approvals: true,
            ..AgentConfig::default()
        },
    )
}

fn send(target: &str, n: u32) -> Reply {
    Reply::Tool("send", json!({ "target": target, "n": n }))
}

async fn drive(h: &Harness, agent: &Agent, trigger: &str) {
    let model: ModelRef = "script:m".parse().unwrap();
    let run = agent
        .start_run(&h.session, "go", &model, "system", trigger)
        .unwrap();
    agent
        .drive(&run.id, CancellationToken::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn always_is_offered_on_the_fourth_ask_not_before() {
    let h = Harness::new();
    let asks = Answer::new(Decision::AllowOnce);
    let script = Arc::new(Script::new(vec![
        send("bob", 1),
        send("bob", 2),
        send("bob", 3),
        send("bob", 4),
        send("amy", 5),
        Reply::Text("ok"),
    ]));
    let a = agent(&h, script, Effect::Write, asks.clone());
    drive(&h, &a, "user").await;
    let asked = asks.asked();
    let always: Vec<bool> = asked
        .iter()
        .map(|r| r.grant.as_ref().unwrap().always)
        .collect();
    // Bob: not until three answers are in; another target starts over.
    assert_eq!(always, [false, false, false, true, false], "{asked:?}");
    assert!(asked[3].notice.as_deref().unwrap().contains("3 times"));
    assert!(asked[0].notice.is_none());
}

#[tokio::test]
async fn an_always_answer_to_an_unlearned_card_is_just_once() {
    let h = Harness::new();
    let asks = Answer::new(Decision::AlwaysAllow);
    let script = Arc::new(Script::new(vec![
        send("bob", 1),
        send("bob", 2),
        Reply::Text("ok"),
    ]));
    let a = agent(&h, script, Effect::Write, asks.clone());
    drive(&h, &a, "user").await;
    assert_eq!(asks.asked().len(), 2, "not granted: asked again");
    assert!(h.store.allowlist("send").unwrap().is_empty());
}

/// Once for the first three, then always.
struct Learner(AtomicU32);

#[async_trait]
impl ApprovalHandler for Learner {
    async fn ask(&self, _: &ApprovalRequest) -> Decision {
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0..=2 => Decision::AllowOnce,
            _ => Decision::AlwaysAllow,
        }
    }
}

#[tokio::test]
async fn learned_always_grants_and_covers_the_next_call() {
    let h = Harness::new();
    let asks = Arc::new(Learner(AtomicU32::new(0)));
    let calls = (1..=5)
        .map(|n| send("bob", n))
        .chain([Reply::Text("ok")])
        .collect();
    let a = agent(
        &h,
        Arc::new(Script::new(calls)),
        Effect::Write,
        asks.clone(),
    );
    drive(&h, &a, "user").await;
    assert_eq!(asks.0.load(Ordering::SeqCst), 4, "the fifth is covered");
    assert_eq!(h.store.allowlist("send").unwrap(), ["key:to bob"]);
}

#[tokio::test]
async fn destructive_and_routine_runs_never_learn() {
    let h = Harness::new();
    let asks = Answer::new(Decision::AllowOnce);
    let calls = (1..=5)
        .map(|n| send("bob", n))
        .chain([Reply::Text("ok")])
        .collect();
    let a = agent(
        &h,
        Arc::new(Script::new(calls)),
        Effect::Destructive,
        asks.clone(),
    );
    drive(&h, &a, "user").await;
    assert!(
        asks.asked()
            .iter()
            .all(|r| r.grant.is_none() && r.pattern.is_none())
    );

    let h = Harness::new();
    let asks = Answer::new(Decision::AllowOnce);
    let calls = (1..=5)
        .map(|n| send("bob", n))
        .chain([Reply::Text("ok")])
        .collect();
    let a = agent(
        &h,
        Arc::new(Script::new(calls)),
        Effect::Write,
        asks.clone(),
    );
    drive(&h, &a, "cron").await;
    let asked = asks.asked();
    assert_eq!(asked.len(), 5);
    assert!(
        asked.iter().all(|r| !r.grant.as_ref().unwrap().always),
        "{asked:?}"
    );
}
