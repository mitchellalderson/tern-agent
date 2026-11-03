import 'dart:convert';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

TernEvent ev(String s) => TernEvent.fromJson(jsonDecode(s) as Json);

void main() {
  test('decodes agent events by type', () {
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
}
