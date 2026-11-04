#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};

use tern_core::script::{HostRequest, ScriptHost};
use tokio_util::sync::CancellationToken;

use super::*;

fn ctx(workspace: &std::path::Path) -> ToolCtx {
    ToolCtx {
        workspace: workspace.to_owned(),
        run_id: "r".into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    }
}

async fn run_code(tool: &Python, code: &str) -> ToolResult {
    let dir = tempfile::tempdir().unwrap();
    tool.call(&ctx(dir.path()), json!({ "code": code })).await
}

async fn py(code: &str) -> ToolResult {
    run_code(&Python::default(), code).await
}

fn lookup_spec() -> ToolSpec {
    ToolSpec {
        name: "lua__county__lookup".into(),
        description: "Look up a property".into(),
        input_schema: json!({
            "type": "object",
            "properties": { "address": { "type": "string" }, "year": { "type": "integer" } },
            "required": ["address"]
        }),
    }
}

/// Run `code` with a host that answers `lua__county__lookup` with a value
/// per address, as the agent loop would. Returns the result and the inputs
/// the host saw.
async fn with_fake_host(tool: &Python, code: &str, untrusted: bool) -> (ToolResult, Vec<Value>) {
    let (host, mut rx) = ScriptHost::channel(
        vec![lookup_spec()],
        BTreeMap::from([(
            "shell".to_owned(),
            "destructive tool; scripts can only call read-only tools".to_owned(),
        )]),
    );
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let served = seen.clone();
    let n = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            match req {
                HostRequest::Env(reply) => {
                    let _ = reply.send(ScriptEnv {
                        clock_secs: 1_800_000_000,
                        seed: 7,
                    });
                }
                HostRequest::Call { name, input, reply } => {
                    assert_eq!(name, "lua__county__lookup");
                    served.lock().unwrap().push(input.clone());
                    let i = n.fetch_add(1, Ordering::SeqCst) as i64;
                    let mut r = if input["address"] == "missing" {
                        ToolResult::error("404 address not found\nmore detail")
                    } else {
                        ToolResult::ok(
                            json!({ "address": input["address"], "value": 100_000 + i * 1000 })
                                .to_string(),
                        )
                    };
                    r.untrusted = untrusted;
                    let _ = reply.send(HostReply::Done(r));
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let out = script::with_host(host, tool.call(&ctx(dir.path()), json!({ "code": code }))).await;
    let seen = seen.lock().unwrap().clone();
    (out, seen)
}

#[tokio::test]
async fn returns_the_last_expression_and_printed_output() {
    let r =
        py("xs = [3, 1, 2]\nprint('sorting', len(xs))\n{'sorted': sorted(xs), 'n': len(xs)}").await;
    assert!(!r.is_error, "{}", r.content);
    assert!(
        r.content.contains("result:\n{\"n\":3,\"sorted\":[1,2,3]}"),
        "{}",
        r.content
    );
    assert!(r.content.contains("printed:\nsorting 3"), "{}", r.content);
}

#[tokio::test]
async fn inputs_become_globals() {
    let dir = tempfile::tempdir().unwrap();
    let r = Python::default()
        .call(
            &ctx(dir.path()),
            json!({ "code": "sum(r['v'] for r in rows)", "inputs": { "rows": [{ "v": 2 }, { "v": 5 }] } }),
        )
        .await;
    assert!(r.content.contains("result:\n7"), "{}", r.content);
}

#[tokio::test]
async fn a_script_calls_a_tool_in_a_loop() {
    let code = r#"
addrs = ["1 Elm St", "2 Elm St", "3 Elm St"]
rows = [lua__county__lookup(a) for a in addrs]
rows.append(lua__county__lookup(address="4 Elm St", year=2025))
values = sorted(r["value"] for r in rows)
{"n": len(rows), "median": values[len(values) // 2]}
"#;
    let (r, seen) = with_fake_host(&Python::default(), code, false).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("\"n\":4"), "{}", r.content);
    assert!(
        r.content.contains("tool calls: lua__county__lookup ×4"),
        "{}",
        r.content
    );
    assert_eq!(seen[0], json!({ "address": "1 Elm St" }));
    assert_eq!(seen[3], json!({ "address": "4 Elm St", "year": 2025 }));
    assert!(!r.untrusted);
}

#[tokio::test]
async fn tool_errors_raise_and_can_be_caught() {
    let code = r#"
out = []
for a in ["1 Elm St", "missing"]:
    try:
        out.append(lua__county__lookup(a)["value"])
    except RuntimeError as e:
        out.append(str(e))
out
"#;
    let (r, _) = with_fake_host(&Python::default(), code, false).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(
        r.content
            .contains("lua__county__lookup: 404 address not found\""),
        "only the first line: {}",
        r.content
    );
    assert!(r.content.contains("×2 (1 failed)"), "{}", r.content);
}

#[tokio::test]
async fn untrusted_results_make_the_output_untrusted() {
    let (r, _) = with_fake_host(&Python::default(), "lua__county__lookup('1 Elm St')", true).await;
    assert!(r.untrusted);
}

#[tokio::test]
async fn bad_arguments_are_a_type_error_before_any_call() {
    let (r, seen) =
        with_fake_host(&Python::default(), "lua__county__lookup(street='x')", false).await;
    assert!(r.is_error);
    assert!(
        r.content.contains("unexpected keyword argument 'street'"),
        "{}",
        r.content
    );
    assert!(r.content.contains("address, year"), "{}", r.content);
    assert!(seen.is_empty());
}

#[tokio::test]
async fn unknown_functions_suggest_the_tool() {
    let (r, seen) = with_fake_host(&Python::default(), "county_lookup('x')", false).await;
    assert!(r.is_error);
    assert!(
        r.content
            .to_lowercase()
            .contains("did you mean lua__county__lookup?"),
        "{}",
        r.content
    );
    assert!(seen.is_empty());
}

#[tokio::test]
async fn the_host_call_cap_stops_the_script() {
    let tool = Python::new(Limits {
        host_calls: 3,
        ..Limits::default()
    });
    let code = "for i in range(10):\n    try:\n        lua__county__lookup(str(i))\n    except Exception:\n        pass\n'done'";
    let (r, seen) = with_fake_host(&tool, code, false).await;
    assert!(r.is_error, "the cap can't be caught: {}", r.content);
    assert!(
        r.content
            .contains("too many tool calls in one script; the limit is 3"),
        "{}",
        r.content
    );
    assert_eq!(seen.len(), 3);
}

#[tokio::test]
async fn awaiting_a_tool_is_explained() {
    let code = "import asyncio\nasync def m():\n    return await asyncio.gather(lua__county__lookup('a'))\nasyncio.run(m())";
    let (r, _) = with_fake_host(&Python::default(), code, false).await;
    assert!(r.is_error);
    // Monty rejects the plain value before the loop ever waits on it.
    assert!(
        r.content.contains("awaitable") || r.content.contains("synchronous"),
        "{}",
        r.content
    );
}

#[tokio::test]
async fn without_a_host_no_tools_are_callable() {
    let r = py("lua__county__lookup('x')").await;
    assert!(r.is_error);
    assert!(
        r.content.contains("No tools can be called"),
        "{}",
        r.content
    );
}

#[tokio::test]
async fn the_clock_and_random_come_from_the_journaled_env() {
    let code = "import datetime, random, time\n(datetime.datetime.now().isoformat(), time.time(), random.random())";
    let (a, _) = with_fake_host(&Python::default(), code, false).await;
    let (b, _) = with_fake_host(&Python::default(), code, false).await;
    assert!(a.content.contains("2027-01-15T08:00:00"), "{}", a.content);
    assert!(a.content.contains("1800000000"), "{}", a.content);
    assert_eq!(a.content, b.content, "a re-run sees the same values");
}

#[tokio::test]
async fn the_workspace_is_readable_but_not_writable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("prices.csv"), "addr,price\na,10\nb,32\n").unwrap();
    let tool = Python::default();
    let r = tool
        .call(
            &ctx(dir.path()),
            json!({ "code": "import csv\nrows = list(csv.DictReader(open('/workspace/prices.csv')))\nsum(int(r['price']) for r in rows)" }),
        )
        .await;
    assert!(r.content.contains("result:\n42"), "{}", r.content);

    let r = tool
        .call(
            &ctx(dir.path()),
            json!({ "code": "open('/workspace/out.txt', 'w').write('x')" }),
        )
        .await;
    assert!(r.is_error, "{}", r.content);
    assert!(!dir.path().join("out.txt").exists());

    let r = tool
        .call(
            &ctx(dir.path()),
            json!({ "code": "open('/etc/passwd').read()" }),
        )
        .await;
    assert!(r.is_error, "{}", r.content);
    assert!(r.content.contains("PermissionError"), "{}", r.content);
}

#[tokio::test]
async fn class_inheritance_gets_a_rewrite_hint() {
    let r = py("class A:\n    pass\nclass B(A):\n    pass\n").await;
    assert!(r.is_error);
    assert!(r.content.contains("NotImplementedError"), "{}", r.content);
    assert!(
        r.content.contains("hint: classes can't inherit"),
        "{}",
        r.content
    );
}

#[tokio::test]
async fn every_feature_hint_still_matches_monty() {
    for (code, hint) in [
        ("def g():\n    yield 1\n", "generators"),
        ("x = [1]\ndel x[0]\n", "`del`"),
    ] {
        let r = py(code).await;
        assert!(r.is_error, "{code}");
        assert!(r.content.contains(hint), "{code}: {}", r.content);
    }
}

#[tokio::test]
async fn missing_modules_say_what_to_do() {
    let r = py("import pandas").await;
    assert!(r.content.contains("ModuleNotFoundError"), "{}", r.content);
    assert!(
        r.content.contains("use shell with python3"),
        "{}",
        r.content
    );
    let r = py("import statistics").await;
    assert!(r.content.contains("median = sorted(xs)"), "{}", r.content);
}

#[tokio::test]
async fn syntax_errors_point_at_the_line() {
    let r = py("x = 1\n  y = 2").await;
    assert!(r.is_error);
    assert!(r.content.contains("line 2"), "{}", r.content);
    assert!(r.content.contains("SyntaxError"), "{}", r.content);
}

#[tokio::test]
async fn limits_end_the_script_cleanly() {
    let tool = Python::new(Limits {
        cpu: Duration::from_millis(200),
        memory: 10 * 1024 * 1024,
        ..Limits::default()
    });
    let r = run_code(&tool, "while True:\n    pass").await;
    assert!(r.is_error);
    assert!(r.content.contains("TimeoutError"), "{}", r.content);
    assert!(
        r.content
            .contains("hint: the script ran out of interpreter time"),
        "{}",
        r.content
    );

    let r = run_code(&tool, "x = 'a' * (10 ** 9)").await;
    assert!(r.content.contains("MemoryError"), "{}", r.content);
    assert!(
        r.content.contains("hint: the script hit its memory limit"),
        "{}",
        r.content
    );

    let r = run_code(&tool, "def f(n):\n    return f(n + 1)\nf(0)").await;
    assert!(r.content.contains("RecursionError"), "{}", r.content);

    let r = run_code(&tool, &"x = 1\n".repeat(20_000)).await;
    assert!(r.is_error);
    assert!(r.content.contains("the limit is"), "{}", r.content);
}

#[tokio::test]
async fn printed_output_is_capped() {
    let tool = Python::new(Limits {
        printed_bytes: 1000,
        ..Limits::default()
    });
    let r = run_code(&tool, "for i in range(5000):\n    print('line', i)\n'ok'").await;
    assert!(r.content.contains("result:\n\"ok\""), "{}", r.content);
    assert!(r.content.len() < 2000, "{}", r.content.len());
}

#[test]
fn values_round_trip_through_json() {
    let v = json!({ "a": [1, 2.5, "x", null, true], "b": { "c": -3 } });
    assert_eq!(convert::to_json(&convert::to_monty(&v)), v);
}
