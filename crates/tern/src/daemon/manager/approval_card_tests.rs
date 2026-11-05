//! Approval cards: identical requests share one, and a scoped answer
//! releases the queued requests it covers.

use super::*;
use tern_core::approval::GrantOffer;
use tern_core::tool::Effect;
use tern_core::types::ToolCall;

fn req(id: &str, run: &str, session: &str, target: &str, n: u32, grant: bool) -> ApprovalRequest {
    ApprovalRequest {
        id: id.into(),
        run_id: run.into(),
        session_id: session.into(),
        call: ToolCall {
            id: format!("c_{id}"),
            name: "connector_test".into(),
            input: serde_json::json!({ "name": target, "n": n }),
        },
        summary: format!("connector_test draft {target}.t"),
        effect: Effect::Read,
        pattern: None,
        parent_run_id: None,
        agent_label: None,
        taint: None,
        local_only: false,
        egress_host: None,
        script: None,
        grant: grant.then(|| GrantOffer {
            key: format!("draft {target}.t#1"),
            label: format!("draft {target}.t"),
        }),
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(30)).await;
}

fn ask(api: &Arc<ApiApprovals>, r: ApprovalRequest) -> tokio::task::JoinHandle<Decision> {
    let api = api.clone();
    tokio::spawn(async move { api.ask(&r, None, (None, None)).await })
}

#[tokio::test]
async fn identical_requests_share_one_card_and_one_answer_covers_them() {
    let api = Arc::new(ApiApprovals::default());
    let mut rx = api.subscribe();
    let b = req("a2", "r1", "s", "x", 1, true);
    let t1 = ask(&api, req("a1", "r1", "s", "x", 1, true));
    settle().await;
    assert!(!api.joins_card(&req("zz", "r1", "s", "other", 1, true)));
    assert!(api.joins_card(&b));
    let t2 = ask(&api, b);
    settle().await;
    let pending = api.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!((pending[0].id.as_str(), pending[0].count), ("a1", 2));
    // Clients are told the count grew, under the same id.
    let first = rx.recv().await.unwrap();
    let second = rx.recv().await.unwrap();
    assert_eq!((first.id.as_str(), first.count), ("a1", 1));
    assert_eq!((second.id.as_str(), second.count), ("a1", 2));
    assert!(api.answer("a1", Decision::AllowOnce, false));
    assert_eq!(t1.await.unwrap(), Decision::AllowOnce);
    assert_eq!(t2.await.unwrap(), Decision::AllowOnce);
    assert!(api.pending().is_empty());
}

#[tokio::test]
async fn different_input_or_run_does_not_share_a_card() {
    let api = Arc::new(ApiApprovals::default());
    let _rx = api.subscribe();
    for r in [
        req("a1", "r1", "s", "x", 1, true),
        req("a2", "r1", "s", "x", 2, true),
        req("a3", "r2", "s", "x", 1, true),
    ] {
        ask(&api, r);
    }
    settle().await;
    assert_eq!(api.pending().len(), 3);
}

#[tokio::test]
async fn a_scoped_answer_releases_queued_requests_with_that_key_only() {
    let api = Arc::new(ApiApprovals::default());
    let _rx = api.subscribe();
    let first = ask(&api, req("a1", "r1", "s", "x", 1, true));
    let same_key = ask(&api, req("a2", "r1", "s", "x", 2, true));
    let other_run = ask(&api, req("a3", "r2", "s", "x", 3, true));
    let other_target = ask(&api, req("a4", "r1", "s", "y", 4, true));
    // No grant offered (tainted): never released.
    let tainted = ask(&api, req("a5", "r1", "s", "x", 5, false));
    settle().await;
    assert_eq!(api.pending().len(), 5);
    assert!(api.answer("a1", Decision::AllowRun, false));
    assert_eq!(first.await.unwrap(), Decision::AllowRun);
    assert_eq!(same_key.await.unwrap(), Decision::CoveredByGrant);
    settle().await;
    let mut left: Vec<String> = api.pending().into_iter().map(|a| a.id).collect();
    left.sort();
    assert_eq!(left, ["a3", "a4", "a5"]);

    // A conversation grant also releases the other run in that session.
    assert!(api.answer("a3", Decision::AllowSession, false));
    assert_eq!(other_run.await.unwrap(), Decision::AllowSession);
    settle().await;
    assert_eq!(api.pending().len(), 2);
    // "Once" releases nothing else.
    assert!(api.answer("a4", Decision::AllowOnce, false));
    assert_eq!(other_target.await.unwrap(), Decision::AllowOnce);
    assert_eq!(api.pending().len(), 1);
    assert!(api.answer("a5", Decision::Deny, false));
    assert_eq!(tainted.await.unwrap(), Decision::Deny);
}

#[tokio::test]
async fn a_remote_answer_never_releases_a_local_only_card() {
    let api = Arc::new(ApiApprovals::default());
    let _rx = api.subscribe();
    let first = ask(&api, req("a1", "r1", "s", "x", 1, true));
    let mut local = req("a2", "r1", "s", "x", 2, true);
    local.local_only = true;
    let local = ask(&api, local);
    settle().await;
    // A paired device answers: the local-only card stays for this host.
    assert!(api.answer("a1", Decision::AllowRun, true));
    assert_eq!(first.await.unwrap(), Decision::AllowRun);
    settle().await;
    assert_eq!(api.pending().len(), 1);
    // The same answer from this host's socket releases it, as covered.
    let again = ask(&api, req("a3", "r1", "s", "x", 3, true));
    let mut local2 = req("a4", "r1", "s", "x", 4, true);
    local2.local_only = true;
    let local2 = ask(&api, local2);
    settle().await;
    assert!(api.answer("a3", Decision::AllowRun, false));
    assert_eq!(again.await.unwrap(), Decision::AllowRun);
    assert_eq!(local2.await.unwrap(), Decision::CoveredByGrant);
    assert_eq!(local.await.unwrap(), Decision::CoveredByGrant);
}

#[tokio::test]
async fn a_request_that_stops_waiting_leaves_its_card_until_the_last_one_does() {
    let api = Arc::new(ApiApprovals::default());
    let _rx = api.subscribe();
    let t1 = ask(&api, req("a1", "r1", "s", "x", 1, true));
    settle().await;
    let t2 = ask(&api, req("a2", "r1", "s", "x", 1, true));
    settle().await;
    t1.abort();
    settle().await;
    let pending = api.pending();
    assert_eq!((pending.len(), pending[0].count), (1, 1));
    t2.abort();
    settle().await;
    assert!(api.pending().is_empty());
}
