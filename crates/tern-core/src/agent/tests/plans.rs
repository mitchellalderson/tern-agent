//! Per-call plans (a tool's policy/grant key decided for one call, used up
//! by exactly that call): scripts' calls find their own, and refused,
//! denied or cancelled calls and ended runs leave none behind.

use std::collections::HashSet;

use super::py_scripts::{call, python_turn, said, writable};
use super::*;
use crate::tool::{GrantKey, current_call_id, planning_scope};

/// A tool that plans per call like `connector_test`: `policy` stores a plan
/// under (run, call), `call` must find and use up its own, `forget_plans`
/// drops them.
struct Planner {
    plans: Mutex<HashSet<(String, String)>>,
    ran: AtomicU32,
    effect: Effect,
}

impl Planner {
    fn new(effect: Effect) -> Arc<Self> {
        Arc::new(Self {
            plans: Mutex::new(HashSet::new()),
            ran: AtomicU32::new(0),
            effect,
        })
    }
    fn held(&self) -> usize {
        self.plans.lock().unwrap().len()
    }
}

#[async_trait]
impl Tool for Planner {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "planner".into(),
            description: "plans".into(),
            input_schema: json!({"type": "object"}),
        }
    }
    fn effect(&self) -> Effect {
        self.effect
    }
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        self.plans
            .lock()
            .unwrap()
            .insert(planning_scope().unwrap_or_default());
        ToolPolicy::new(self.effect, false).forced(true)
    }
    fn allow_always(&self) -> bool {
        false
    }
    fn grant_key(&self, _: &Value) -> Option<GrantKey> {
        Some(GrantKey {
            key: "planner#1".into(),
            label: "planner".into(),
            ..GrantKey::default()
        })
    }
    fn forget_plans(&self, run_id: &str, call_id: Option<&str>) {
        self.plans
            .lock()
            .unwrap()
            .retain(|(r, c)| !(r == run_id && call_id.is_none_or(|id| id == c)));
    }
    async fn call(&self, ctx: &ToolCtx, _: Value) -> ToolResult {
        let me = (ctx.run_id.clone(), current_call_id().unwrap_or_default());
        if !self.plans.lock().unwrap().remove(&me) {
            return ToolResult::error("unplanned: no plan for this call id");
        }
        self.ran.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("ran")
    }
}

#[tokio::test]
async fn a_scripts_calls_each_find_their_own_plan_including_a_gather_under_a_grant() {
    let h = Harness::new();
    let planner = Planner::new(Effect::Write);
    let answer = Answer::new(Decision::AllowRun);
    let calls: Vec<Value> = (0..3)
        .map(|_| call("planner", json!({"same": 1})))
        .collect();
    let script = Arc::new(Script::new(vec![
        python_turn(json!(calls), true),
        Reply::Text("done"),
    ]));
    let agent = super::py_scripts::agent_limited(
        &h,
        script.clone(),
        vec![planner.clone(), super::py_scripts::Driver::new()],
        answer.clone(),
        writable(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    // Approved once for the run; the other identical calls are covered by
    // that grant. Every one ran under its own plan, none unplanned.
    assert_eq!(said(&script), "ran\nran\nran");
    assert_eq!(planner.ran.load(Ordering::SeqCst), 3);
    assert_eq!(answer.asked().len(), 1);
    assert_eq!(planner.held(), 0);
}

#[tokio::test]
async fn a_denied_call_and_an_ended_run_leave_no_plans() {
    // Denied directly, and denied from a script.
    let h = Harness::new();
    let planner = Planner::new(Effect::Write);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("planner", json!({})),
        python_turn(json!([call("planner", json!({}))]), false),
        Reply::Text("done"),
    ]));
    let agent = super::py_scripts::agent_limited(
        &h,
        script,
        vec![planner.clone(), super::py_scripts::Driver::new()],
        Answer::new(Decision::Deny),
        writable(),
    );
    let run = h.start(&agent, "go");
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(planner.ran.load(Ordering::SeqCst), 0);
    assert_eq!(planner.held(), 0, "denied calls forgot their plans");

    // Refused as read-only.
    let h = Harness::new();
    let planner = Planner::new(Effect::Write);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("planner", json!({})),
        Reply::Text("done"),
    ]));
    let agent = h.agent(
        script,
        vec![planner.clone()],
        Answer::new(Decision::AllowOnce),
    );
    let run = h.start(&agent, "go");
    h.store.set_run_autonomy(&run, "readonly").unwrap();
    agent.drive(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(planner.held(), 0, "refused calls forgot their plans");

    // Cancelled while waiting for the user: the run's end drops its plans.
    struct Never;
    #[async_trait]
    impl ApprovalHandler for Never {
        async fn ask(&self, _: &ApprovalRequest) -> Decision {
            std::future::pending().await
        }
    }
    let h = Harness::new();
    let planner = Planner::new(Effect::Write);
    let script = Arc::new(Script::new(vec![
        Reply::Tool("planner", json!({})),
        Reply::Text("done"),
    ]));
    let agent = h.agent(script, vec![planner.clone()], Arc::new(Never));
    let run = h.start(&agent, "go");
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        stop.cancel();
    });
    agent.drive(&run, cancel).await.unwrap();
    assert_eq!(planner.ran.load(Ordering::SeqCst), 0);
    assert_eq!(planner.held(), 0, "an ended run leaves no plans");
}
