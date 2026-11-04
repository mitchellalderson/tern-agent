import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

TernEvent ev(String s) => TernEvent.fromJson(jsonDecode(s) as Json);

void main() {
  test('decodes agent events by type', () {
    expect(
      ev(
        '{"type":"run_finished","run_id":"r","status":"completed","error":null,"quiet":true}',
      ),
      isA<RunFinished>().having((e) => e.quiet, 'quiet', isTrue),
    );
    expect(
      ev(
        '{"type":"run_finished","run_id":"r","status":"completed","error":null}',
      ),
      isA<RunFinished>().having((e) => e.quiet, 'quiet', isFalse),
    );
    expect(
      ev('{"type":"run_started","run_id":"r","session_id":"s"}'),
      isA<RunStarted>().having((e) => e.sessionId, 'session', 's'),
    );
    expect(
      ev(
        '{"type":"tool_finished","run_id":"r","id":"t","name":"shell",'
        '"is_error":true,"preview":"p","output":"o","replayed":false}',
      ),
      isA<ToolFinished>().having((e) => e.isError, 'error', true),
    );
    expect(
      ev(
        '{"type":"usage","run_id":"r","usage":{"input_tokens":3,'
        '"output_tokens":4}}',
      ),
      isA<UsageEvent>().having((e) => e.usage.total, 'total', 7),
    );
  });

  test('decodes approval requests', () {
    final e = ev(
      '{"approval_requested":{"id":"a","run_id":"r",'
      '"session_id":"s","tool":"shell","input":{"command":"ls"},'
      '"summary":"shell ls","effect":"destructive","pattern":"ls"}}',
    );
    expect(e, isA<ApprovalRequested>());
    final a = (e as ApprovalRequested).approval;
    expect(a.summary, 'shell ls');
    expect(a.headline, 'destructive · shell ls');
    // Daemons before T0 send no effect.
    final old = ev(
      '{"approval_requested":{"id":"a","run_id":"r",'
      '"session_id":"s","tool":"t","input":{},"summary":"t","pattern":null}}',
    );
    expect((old as ApprovalRequested).approval.effect, 'write');
  });

  test('decodes the host-limit prompt and its answers', () {
    final e = ev(
      '{"approval_requested":{"id":"a","run_id":"r",'
      '"session_id":"s","tool":"http_fetch","input":{},'
      '"summary":"http_fetch https://b.example","effect":"read","pattern":null,'
      '"taint":"result of http_fetch","egress_host":"b.example"}}',
    );
    expect((e as ApprovalRequested).approval.egressHost, 'b.example');
    expect(Decision.run.wire, 'run');
    expect(Decision.host.wire, 'host');
  });

  test('decodes taint on approvals and runs', () {
    final e = ev(
      '{"approval_requested":{"id":"a","run_id":"r",'
      '"session_id":"s","tool":"write_file","input":{},'
      '"summary":"write_file a","effect":"write","pattern":null,'
      '"taint":"result of http_fetch"}}',
    );
    final a = (e as ApprovalRequested).approval;
    expect(a.taint, 'result of http_fetch');
    expect(
      a.taintNote,
      'this run read untrusted content (result of http_fetch)',
    );
    final run = RunItem.fromJson(
      jsonDecode(
            '{"id":"r","status":"completed","trigger":"cron","model":"m",'
            '"started_at":1762185600000,"finished_at":null,"error":null,'
            '"taint":"result of mcp_list"}',
          )
          as Json,
    );
    expect(run.tainted, isTrue);
    // Clean runs, and daemons before T3a, send none.
    final clean = RunItem.fromJson(
      jsonDecode(
            '{"id":"r","status":"completed","trigger":"user","model":"m",'
            '"started_at":1762185600000,"finished_at":null,"error":null}',
          )
          as Json,
    );
    expect(clean.tainted, isFalse);
  });

  test('decodes tool effects', () {
    final t = ToolSpec.fromJson({
      'name': 'http_fetch',
      'description': 'GET',
      'input_schema': {'type': 'object'},
      'effect': 'read',
      'untrusted': true,
    });
    expect((t.effect, t.untrusted), ('read', true));
    final old = ToolSpec.fromJson({'name': 'x', 'description': ''});
    expect((old.effect, old.untrusted), ('write', false));
  });

  test('unknown events survive', () {
    expect(ev('{"type":"from_the_future","run_id":"r"}'), isA<UnknownEvent>());
  });

  test('decodes session messages', () {
    final d = SessionDetail.fromJson(
      jsonDecode(
            '{"id":"s","title":null,'
            '"model":"m","messages":[{"role":"assistant","summary":false,'
            '"content":[{"type":"text","text":"hi"},{"type":"tool_use",'
            '"id":"t","name":"shell","input":{}}]},{"role":"user",'
            '"summary":false,"content":[{"type":"tool_result",'
            '"tool_use_id":"t","content":"ok"}]}]}',
          )
          as Json,
    );
    expect(d.messages, hasLength(2));
    expect(d.messages[0].content[1], isA<ToolUseBlock>());
    expect((d.messages[1].content[0] as ToolResultBlock).isError, isFalse);
  });

  test('events carry their session when the daemon sends it', () {
    final e = ev(
      '{"type":"text_delta","run_id":"r","session_id":"s","text":"x"}',
    );
    expect((e as RunEvent).sessionId, 's');
    expect(
      (ev('{"type":"text_delta","run_id":"r","text":"x"}') as RunEvent)
          .sessionId,
      isNull,
    );
  });

  test('decodes schedules, runs and steps', () {
    final s = Schedule.fromJson(
      jsonDecode(
            '{"id":"sch_1","cron":"0 8 * * 1-5",'
            '"prompt":"p","session_id":"s","deliver_to":null,"enabled":false,'
            '"next_run_at":null,"last_run_at":1762185600000,"last_status":"completed"}',
          )
          as Json,
    );
    expect(s.nextRunAt, isNull);
    expect(s.lastRunAt!.year, 2025);
    // An older daemon sends no T1a fields.
    expect(s.lastQuiet, isFalse);
    expect(s.limits.quietHours, isNull);
    expect(s.pausedByBudget, isFalse);

    // The shape the daemon sends today (checked against it in Rust).
    final t1a = Schedule.fromJson(
      jsonDecode(File('test/fixtures/schedule.json').readAsStringSync())
          as Json,
    );
    expect(t1a.lastQuiet, isTrue);
    expect(t1a.limits.quietHours, '23:00-06:30');
    expect(t1a.limits.urgent, isFalse);
    expect(t1a.limits.runsPerDay, 12);
    expect(t1a.limits.usdPerDay, 0.5);
    expect(t1a.limits.tokensPerRun, 200000);
    expect(t1a.pausedByBudget, isTrue);
    expect(t1a.pausedReason, 'runs_per_day');
    expect(t1a.pausedUntil, isNotNull);
    expect(t1a.trigger, 'cron');
    expect(t1a.initiative, 'interrupt');
    expect(t1a.lastCheck, 'gate: passed');
    expect(t1a.gate, contains('wday'));
    expect(
      const RoutineOptions(watch: 'github.reviews', every: '5m').toJson(),
      {
        'watch': 'github.reviews',
        'every': '5m',
        'initiative': 'default',
        'gate': null,
        'gate_connector': null,
      },
    );
    expect(const ScheduleLimits(runsPerDay: 3).toJson(), {
      'quiet_hours': null,
      'urgent': false,
      'runs_per_day': 3,
      'usd_per_day': null,
      'tokens_per_run': null,
    });
    final step = StepItem.fromJson(
      jsonDecode(
            '{"idx":0,"kind":"llm","status":"completed",'
            '"input":{"model":"m"},"output":null,"usage":{"input_tokens":5,"output_tokens":1},'
            '"error":null,"started_at":1762185600000,"finished_at":1762185602500}',
          )
          as Json,
    );
    expect(step.duration, const Duration(milliseconds: 2500));
    expect(step.usage!.total, 6);
  });

  test('decodes secrets and validates names', () {
    final s = SecretStatus.fromJson(
      jsonDecode(
            '{"name":"ANTHROPIC_API_KEY","in_file":false,"in_env":true,'
            '"used_by":["provider anthropic"]}',
          )
          as Json,
    );
    expect(s.isSet, isTrue);
    expect(s.usedBy, ['provider anthropic']);
    expect(secretName.hasMatch('GITHUB_TOKEN'), isTrue);
    expect(secretName.hasMatch('9LIVES'), isFalse);
    expect(secretName.hasMatch('A-B'), isFalse);
  });

  test('decodes config settings', () {
    final v = ConfigView.fromJson(
      jsonDecode(
            '{"path":"/h/.tern/config.toml","sections":[{"id":"sandbox",'
            '"title":"Shell & sandbox","settings":[{"key":"sandbox.network",'
            '"kind":"bool","optional":false,"value":true,"default":true,'
            '"set":true,"description":"d","applies":"restart",'
            '"restart_pending":false,"security":"s","risky":true,'
            '"editable":true},{"key":"sandbox.docker_cpus","kind":"float",'
            '"min":0.0,"optional":false,"value":1.5,"default":2.0,"set":true,'
            '"description":"","applies":"live","restart_pending":true,'
            '"risky":false,"editable":true},{"key":"x.y","kind":"future",'
            '"value":{},"default":{},"set":false,"applies":"restart",'
            '"restart_pending":false,"risky":false,"editable":false}]}]}',
          )
          as Json,
    );
    final [net, cpus, odd] = v.settings.toList();
    expect(net.kind, SettingKind.boolean);
    expect((net.risky, net.live, net.changed), (true, false, false));
    expect(net.name, 'network');
    expect((cpus.min, cpus.live, cpus.changed), (0.0, true, true));
    expect(cpus.restartPending, isTrue);
    expect(odd.kind, SettingKind.other);
    expect(v.error, isNull);
    expect(
      ConfigView.fromJson({'path': 'p', 'error': 'bad'}).sections,
      isEmpty,
    );
  });

  test('decodes status and shutdown replies', () {
    final s = DaemonStatus.fromJson(
      jsonDecode(
            '{"version":"0.1.0","pid":42,"uptime_secs":90,'
            '"managed_by":"launchd","running_runs":1,"pending_approvals":2,'
            '"active_schedules":3,"gateways":[{"name":"telegram","ok":true}]}',
          )
          as Json,
    );
    expect(s.uptime, const Duration(seconds: 90));
    expect(s.managedBy, 'launchd');
    expect((s.runningRuns, s.pendingApprovals, s.activeSchedules), (1, 2, 3));
    expect(s.gateways.single.ok, isTrue);
    final r = ShutdownReply.fromJson(
      jsonDecode(
            '{"stopping":false,"runs":[{"run_id":"r","session_id":"s",'
            '"title":"Summarize the Q3 report"}]}',
          )
          as Json,
    );
    expect(r.stopping, isFalse);
    expect(r.runs.single.title, 'Summarize the Q3 report');
  });

  test('a webhook decodes as the daemon sends it (T1c)', () {
    // Checked against the Rust API in api_webhooks.rs.
    final w = Webhook.fromJson(
      jsonDecode(File('test/fixtures/webhook.json').readAsStringSync()) as Json,
    );
    expect(w.routineId, 'sch_7c1e9a3f0b2d4e5');
    expect(w.url, 'https://hooks.example.com/v1/hooks/sch_7c1e9a3f0b2d4e5');
    expect(w.active, isTrue);
    expect(w.secret, startsWith('tern_whk_'));
    expect(w.rotatedAt, isNull);
    expect(w.lastDeliveryAt, isNotNull);
    expect(w.howToSign, contains('X-Hub-Signature-256'));
  });
}
