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
      '"summary":"shell ls","pattern":"ls"}}',
    );
    expect(e, isA<ApprovalRequested>());
    expect((e as ApprovalRequested).approval.summary, 'shell ls');
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
}
