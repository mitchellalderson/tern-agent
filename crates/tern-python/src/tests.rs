#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};

use tern_core::script::{HostRequest, ScriptHost};
use tokio_util::sync::CancellationToken;

use super::*;

fn ctx(workspace: &std::path::Path) -> ToolCtx {
    ToolCtx {
        scope: tern_core::workspace::WorkspaceScope::for_dir(workspace.to_owned()),
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
    let (out, seen, _) = with_batches(tool, code, untrusted).await;
    (out, seen)
}

/// As [`with_fake_host`], also returning the size of each batch of calls
/// the interpreter sent (several at once for `gather`).
async fn with_batches(
    tool: &Python,
    code: &str,
    untrusted: bool,
) -> (ToolResult, Vec<Value>, Vec<usize>) {
    let (host, mut rx) = ScriptHost::channel(
        vec![lookup_spec()],
        BTreeMap::from([(
            "shell".to_owned(),
            "destructive tool; scripts can only call read-only tools".to_owned(),
        )]),
    );
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let served = seen.clone();
    let batches = Arc::new(std::sync::Mutex::new(Vec::new()));
    let batched = batches.clone();
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
                HostRequest::Calls { calls, reply } => {
                    batched.lock().unwrap().push(calls.len());
                    let mut answers = Vec::new();
                    for (name, input) in calls {
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
                        if untrusted {
                            r = tern_core::fetch::ingest(tern_core::tool::Ingestion::Internal, r);
                        }
                        answers.push(HostReply::Done(r));
                    }
                    let _ = reply.send(answers);
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let out = script::with_host(host, tool.call(&ctx(dir.path()), json!({ "code": code }))).await;
    let seen = seen.lock().unwrap().clone();
    let batches = batches.lock().unwrap().clone();
    (out, seen, batches)
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
    assert!(!r.untrusted());
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
    assert!(r.untrusted());
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
async fn gather_runs_awaited_tool_calls_together() {
    let code = r#"
import asyncio
async def main():
    rows = await asyncio.gather(*[lua__county__lookup(a) for a in ["a", "b", "c", "d"]])
    return [r["address"] for r in rows]
await main()
"#;
    let (r, seen, batches) = with_batches(&Python::default(), code, false).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(
        r.content.contains(
            r#"result:
["a","b","c","d"]"#
        ),
        "{}",
        r.content
    );
    assert_eq!(seen.len(), 4);
    // One batch of four: the loop can run them in parallel.
    assert_eq!(batches, vec![4]);
    assert!(
        r.content.contains("lua__county__lookup ×4"),
        "{}",
        r.content
    );
}

#[tokio::test]
async fn an_immediate_await_is_a_plain_call() {
    let code = "async def one(a):\n    return await lua__county__lookup(a)\nr = await one('x')\nr['address']";
    let (r, seen, batches) = with_batches(&Python::default(), code, false).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("result:\n\"x\""), "{}", r.content);
    assert_eq!((seen.len(), batches), (1, vec![1]));
}

#[tokio::test]
async fn a_failing_call_in_gather_can_be_caught_per_task() {
    let code = r#"
import asyncio
async def safe(a):
    try:
        return (await lua__county__lookup(a))["address"]
    except RuntimeError:
        return "err"
await asyncio.gather(safe("a"), safe("missing"), safe("c"))
"#;
    let (r, _, batches) = with_batches(&Python::default(), code, false).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains(r#"["a","err","c"]"#), "{}", r.content);
    assert!(r.content.contains("(1 failed)"), "{}", r.content);
    assert_eq!(batches, vec![3]);
}

#[tokio::test]
async fn an_uncaught_failure_in_gather_raises_and_return_exceptions_is_hinted() {
    let code = "import asyncio\nawait asyncio.gather(lua__county__lookup('missing'), lua__county__lookup('b'))";
    let (r, _, _) = with_batches(&Python::default(), code, false).await;
    assert!(r.is_error);
    assert!(r.content.contains("RuntimeError"), "{}", r.content);
    let code =
        "import asyncio\nawait asyncio.gather(lua__county__lookup('a'), return_exceptions=True)";
    let (r, _, _) = with_batches(&Python::default(), code, false).await;
    assert!(
        r.content.contains("catches its own errors"),
        "{}",
        r.content
    );
}

#[tokio::test]
async fn a_bad_call_inside_gather_fails_when_awaited() {
    let code = r#"
import asyncio
async def go():
    return await lua__county__lookup(street="x")
try:
    await asyncio.gather(go(), go())
    out = "no error"
except TypeError as e:
    out = "TypeError"
out
"#;
    let (r, seen, _) = with_batches(&Python::default(), code, false).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("TypeError"), "{}", r.content);
    assert!(seen.is_empty());
}

#[tokio::test]
async fn a_script_without_async_gets_values() {
    // The words only in a string and a comment don't switch modes.
    let code = "# await asyncio\nr = lua__county__lookup('x')\nprint('await')\nr['address']";
    let (r, _, batches) = with_batches(&Python::default(), code, false).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("result:\n\"x\""), "{}", r.content);
    assert_eq!(batches, vec![1]);
}

#[tokio::test]
async fn async_scripts_can_sleep_without_waiting() {
    let code = "import asyncio\nawait asyncio.sleep(5)\n'done'";
    let t = std::time::Instant::now();
    let r = py(code).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("done"), "{}", r.content);
    assert!(t.elapsed() < std::time::Duration::from_secs(4));
}

#[tokio::test]
async fn the_host_call_cap_counts_gathered_calls() {
    let tool = Python::new(Limits {
        host_calls: 3,
        ..Limits::default()
    });
    let code =
        "import asyncio\nawait asyncio.gather(*[lua__county__lookup(str(i)) for i in range(10)])";
    let (r, seen) = with_fake_host(&tool, code, false).await;
    assert!(r.is_error);
    assert!(r.content.contains("too many tool calls"), "{}", r.content);
    assert!(seen.len() <= 3);
}

#[tokio::test]
async fn an_abort_ends_the_script_and_cannot_be_caught() {
    let (host, mut rx) = ScriptHost::channel(vec![lookup_spec()], BTreeMap::new());
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            match req {
                HostRequest::Env(reply) => {
                    let _ = reply.send(ScriptEnv {
                        clock_secs: 1,
                        seed: 1,
                    });
                }
                HostRequest::Calls { calls, reply } => {
                    let _ = reply.send(
                        calls
                            .iter()
                            .map(|_| HostReply::Abort("script changed on resume".into()))
                            .collect(),
                    );
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let code =
        "try:\n    lua__county__lookup('a')\nexcept Exception:\n    print('caught')\n'after'";
    let r = script::with_host(
        host,
        Python::default().call(&ctx(dir.path()), json!({ "code": code })),
    )
    .await;
    assert!(r.is_error);
    assert!(
        r.content.contains("script changed on resume"),
        "{}",
        r.content
    );
    assert!(!r.content.contains("caught") && !r.content.contains("after"));
}

#[tokio::test]
async fn statistics_is_provided() {
    let code = r#"
import statistics
from statistics import mean, median
xs = [2, 4, 4, 4, 5, 5, 7, 9]
{
  "mean": statistics.mean(xs),
  "median": median(xs),
  "mode": statistics.mode(xs),
  "pstdev": statistics.pstdev(xs),
  "stdev": round(statistics.stdev(xs), 4),
  "odd": mean([1, 2, 3]),
  "frac": mean([1, 2]),
  "q": statistics.quantiles([1, 2, 3, 4, 5, 6, 7, 8, 9], n=4),
  "multi": statistics.multimode([1, 1, 2, 2, 3]),
  "words": statistics.mode(["a", "b", "b"]),
}
"#;
    let r = py(code).await;
    assert!(!r.is_error, "{}", r.content);
    for want in [
        r#""mean":5"#,
        r#""median":4.5"#,
        r#""mode":4"#,
        r#""pstdev":2.0"#,
        r#""stdev":2.1381"#,
        r#""odd":2"#,
        r#""frac":1.5"#,
        r#""q":[2.5,5.0,7.5]"#,
        r#""multi":[1,2]"#,
        r#""words":"b""#,
    ] {
        assert!(r.content.contains(want), "{want} in {}", r.content);
    }
}

#[tokio::test]
async fn statistics_errors_are_python_errors() {
    let r = py("import statistics\nstatistics.mean([])").await;
    assert!(r.is_error);
    assert!(r.content.contains("ValueError"), "{}", r.content);
    let r = py("import statistics\ntry:\n    statistics.stdev([1])\nexcept ValueError as e:\n    ok = 'two data points' in str(e)\nok").await;
    assert!(r.content.contains("true"), "{}", r.content);
    let r = py("import statistics\nstatistics.mean(['a'])").await;
    assert!(r.content.contains("TypeError"), "{}", r.content);
}

#[tokio::test]
async fn string_constants_are_provided() {
    let r =
        py("import string\n[string.digits, string.ascii_lowercase[:3], len(string.punctuation)]")
            .await;
    assert!(!r.is_error, "{}", r.content);
    assert!(
        r.content.contains(r#"["0123456789","abc",32]"#),
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
        (
            "import asyncio\nasync def f():\n    return 1\nawait asyncio.gather(f(), return_exceptions=True)\n",
            "catches its own errors",
        ),
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
    let r = py("import enum").await;
    assert!(r.content.contains("plain constants"), "{}", r.content);
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

/// A host whose one answer takes 700 ms, `counted` or not as time spent
/// waiting for the user.
async fn slow_host(tool: &Python, counted: bool) -> ToolResult {
    let (host, mut rx) = ScriptHost::channel(vec![lookup_spec()], BTreeMap::new());
    let waiting = host.clone();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            match req {
                HostRequest::Env(reply) => {
                    let _ = reply.send(ScriptEnv {
                        clock_secs: 1,
                        seed: 1,
                    });
                }
                HostRequest::Calls { calls, reply } => {
                    let delay = Duration::from_millis(700);
                    // (The loop adds a wait after it, but doesn't poll the
                    // script meanwhile; here the host is a separate task.)
                    if counted {
                        waiting.add_wait(delay);
                    }
                    tokio::time::sleep(delay).await;
                    let _ = reply.send(
                        calls
                            .iter()
                            .map(|_| HostReply::Done(ToolResult::ok("{}")))
                            .collect(),
                    );
                }
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    script::with_host(
        host,
        tool.call(
            &ctx(dir.path()),
            json!({ "code": "lua__county__lookup('a')\n'done'" }),
        ),
    )
    .await
}

#[tokio::test]
async fn time_spent_waiting_for_the_user_doesnt_count_against_the_wall_limit() {
    let tool = Python::new(Limits {
        wall: Duration::from_millis(400),
        ..Limits::default()
    });
    let r = slow_host(&tool, true).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("done"), "{}", r.content);
    // The same delay not counted as a wait is a tool call that took too long.
    let r = slow_host(&tool, false).await;
    assert!(r.is_error);
    assert!(r.content.contains("tool calls included"), "{}", r.content);
}
