use std::sync::Mutex as StdMutex;

use tern_core::approval::{ApprovalRequest, Decision, SecretValues};
use tokio_util::sync::CancellationToken;

use super::*;

/// Answers every prompt with `values` (or `None`) and records the requests.
struct Fake {
    values: Option<SecretValues>,
    hang: bool,
    seen: StdMutex<Vec<Request>>,
}

impl Fake {
    fn new(values: Option<&[(&str, &str)]>) -> Arc<Self> {
        Arc::new(Self {
            values: values.map(|v| {
                v.iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect()
            }),
            hang: false,
            seen: StdMutex::default(),
        })
    }

    fn asked(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl ApprovalHandler for Fake {
    async fn ask(&self, _: &ApprovalRequest) -> Decision {
        Decision::Deny
    }
    async fn ask_secrets(&self, req: &Request) -> Option<SecretValues> {
        self.seen.lock().unwrap().push(req.clone());
        if self.hang {
            std::future::pending::<()>().await;
        }
        self.values.clone()
    }
}

struct Env {
    _dir: tempfile::TempDir,
    secrets: Secrets,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let secrets = Secrets::load(&dir.path().join("secrets.toml")).unwrap();
    Env { _dir: dir, secrets }
}

fn tool(env: &Env, asker: Arc<dyn ApprovalHandler>) -> SecretRequest {
    let uses: SecretUses = Arc::new(|| {
        vec![(
            "TERN_TEST_WEATHER".to_owned(),
            "connector weather (draft) · reaches api.weather.com".to_owned(),
        )]
    });
    SecretRequest::new(
        asker,
        env.secrets.clone(),
        Store::open_in_memory().unwrap(),
        uses,
        Duration::from_millis(200),
    )
}

fn ctx(run: &str) -> ToolCtx {
    ToolCtx {
        workspace: std::env::temp_dir(),
        run_id: run.into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    }
}

fn ask(names: &[&str]) -> Value {
    json!({ "names": names, "reason": "the weather connector needs it" })
}

#[tokio::test]
async fn saves_the_value_and_returns_only_the_outcome() {
    let env = env();
    let fake = Fake::new(Some(&[(
        "TERN_TEST_WEATHER",
        "  \"Bearer wk-abc123secret\"\n",
    )]));
    let t = tool(&env, fake.clone());
    let r = t.call(&ctx("r1"), ask(&["TERN_TEST_WEATHER"])).await;
    assert!(!r.is_error, "{}", r.content);
    assert_eq!(r.content, r#"{"TERN_TEST_WEATHER":"set"}"#);
    assert!(!r.content.contains("abc123"));
    // Cleaned up like any pasted key.
    assert_eq!(
        env.secrets.get("TERN_TEST_WEATHER").unwrap(),
        "wk-abc123secret"
    );

    let asked = fake.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].reason, "the weather connector needs it");
    assert_eq!(
        asked[0].asks[0].used_by,
        ["connector weather (draft) · reaches api.weather.com"]
    );
    assert!(!asked[0].asks[0].in_file);
}

#[tokio::test]
async fn already_set_names_are_not_asked_for_unless_replacing() {
    let env = env();
    env.secrets.set("TERN_TEST_WEATHER", "old-value").unwrap();
    let fake = Fake::new(Some(&[("TERN_TEST_WEATHER", "new-value")]));
    let t = tool(&env, fake.clone());

    let r = t.call(&ctx("r1"), ask(&["TERN_TEST_WEATHER"])).await;
    assert_eq!(r.content, r#"{"TERN_TEST_WEATHER":"already_set"}"#);
    assert!(fake.asked().is_empty());

    let mut a = ask(&["TERN_TEST_WEATHER"]);
    a["replace"] = json!(true);
    let r = t.call(&ctx("r1"), a).await;
    assert_eq!(r.content, r#"{"TERN_TEST_WEATHER":"set"}"#);
    assert!(fake.asked()[0].asks[0].in_file);
    assert_eq!(env.secrets.get("TERN_TEST_WEATHER").unwrap(), "new-value");
}

#[tokio::test]
async fn skipped_names_are_not_asked_again_in_the_same_run() {
    let env = env();
    // The answer leaves B out (skipped) and names something not asked for.
    let fake = Fake::new(Some(&[
        ("TERN_TEST_A", "value-a"),
        ("TERN_TEST_NOT_ASKED", "sneaky"),
    ]));
    let t = tool(&env, fake.clone());
    let r = t
        .call(&ctx("r1"), ask(&["TERN_TEST_A", "TERN_TEST_B"]))
        .await;
    assert!(
        r.content
            .starts_with(r#"{"TERN_TEST_A":"set","TERN_TEST_B":"skipped"}"#)
    );
    assert!(r.content.contains("Don't ask for it in the chat"));
    assert!(env.secrets.get("TERN_TEST_NOT_ASKED").is_none());

    let r = t.call(&ctx("r1"), ask(&["TERN_TEST_B"])).await;
    assert!(r.content.starts_with(r#"{"TERN_TEST_B":"skipped"}"#));
    assert_eq!(fake.asked().len(), 1, "no second prompt");

    // Another run may ask again.
    t.call(&ctx("r2"), ask(&["TERN_TEST_B"])).await;
    assert_eq!(fake.asked().len(), 2);
}

#[tokio::test]
async fn nobody_to_ask_and_timeouts() {
    let env = env();
    let t = tool(&env, Fake::new(None));
    let r = t.call(&ctx("r1"), ask(&["TERN_TEST_A"])).await;
    assert!(r.content.starts_with(r#"{"TERN_TEST_A":"skipped"}"#));
    assert!(r.content.contains("isn't interactive"));

    let hang = Arc::new(Fake {
        values: None,
        hang: true,
        seen: StdMutex::default(),
    });
    let t = tool(&env, hang);
    let r = t.call(&ctx("r1"), ask(&["TERN_TEST_A"])).await;
    assert!(r.content.starts_with(r#"{"TERN_TEST_A":"timed_out"}"#));
}

#[tokio::test]
async fn bad_arguments_and_the_per_run_limit() {
    let env = env();
    let t = tool(&env, Fake::new(Some(&[])));
    let r = t
        .call(&ctx("r1"), json!({"names": ["bad name"], "reason": "x"}))
        .await;
    assert!(r.is_error);
    let r = t
        .call(&ctx("r1"), json!({"names": ["OK_NAME"], "reason": " "}))
        .await;
    assert!(r.is_error);
    let six: Vec<String> = (0..6).map(|i| format!("TERN_TEST_{i}")).collect();
    let r = t
        .call(&ctx("r1"), json!({"names": six, "reason": "x"}))
        .await;
    assert!(r.is_error);

    for i in 0..MAX_PROMPTS_PER_RUN {
        let r = t.call(&ctx("r9"), ask(&[&format!("TERN_TEST_N{i}")])).await;
        assert!(!r.is_error);
    }
    let r = t.call(&ctx("r9"), ask(&["TERN_TEST_LAST"])).await;
    assert!(r.is_error, "{}", r.content);
}
