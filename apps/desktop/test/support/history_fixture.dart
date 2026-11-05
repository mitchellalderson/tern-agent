// A long Home history for the windowing tests and the benchmark.
import 'package:tern_client/tern_client.dart';

final historyT0 = DateTime(2026, 11, 3, 9);

/// [n] messages, newest first: per turn a user line, a long assistant
/// answer with a code block, a tool call and its result.
List<StreamItem> longHistory(int n) {
  StreamMessage msg(int id, String role, List<ContentBlock> content) =>
      StreamMessage(
        sessionId: 's1',
        messageId: id,
        message: SessionMessage(
          role: role,
          runId: 'r1',
          content: content,
          summary: false,
        ),
        createdAt: historyT0.add(Duration(seconds: id)),
      );
  final out = <StreamItem>[];
  for (var id = 1; id <= n; id++) {
    out.add(switch (id % 4) {
      1 => msg(id, 'user', [TextBlock('Question number $id about invoices')]),
      2 => msg(id, 'assistant', [
        TextBlock(
          'Answer $id.\n\n${'The invoice total is **310 EUR**. ' * 20}\n\n'
          '```\nline one\nline two\n```\n\n- first\n- second',
        ),
      ]),
      3 => msg(id, 'assistant', [
        ToolUseBlock(id: 'c$id', name: 'list_dir', input: {'path': '.'}),
      ]),
      _ => msg(id, 'user', [
        ToolResultBlock(
          toolUseId: 'c${id - 1}',
          content: List.generate(80, (i) => 'file_$i.txt').join('\n'),
          isError: false,
        ),
      ]),
    });
  }
  return out.reversed.toList();
}

class PerfClient extends TernClient {
  PerfClient(this.items) : super(const Endpoint.unix('/nonexistent'));

  /// Newest first, as the daemon sends them.
  final List<StreamItem> items;
  var streamCalls = 0;
  var lastLimit = 0;

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      mainSessionId: 's1',
      updatedAt: historyT0,
    ),
  ];

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async {
    streamCalls++;
    lastLimit = limit;
    final start = before == null ? 0 : int.parse(before);
    final page = items.skip(start).take(limit).toList();
    final end = start + page.length;
    return StreamPage(items: page, next: end < items.length ? '$end' : null);
  }

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [];
}
