// Against a real daemon running the mock provider. Needs the tern binary:
//   TERN_BIN=../../../../target/debug/tern dart test test/live_test.dart
// Skipped when TERN_BIN isn't set.
@Tags(['live'])
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

void main() {
  final bin = Platform.environment['TERN_BIN'];
  late Directory tmp;
  late Process daemon;
  late TernClient client;

  setUpAll(() async {
    if (bin == null) return;
    tmp = await Directory.systemTemp.createTemp('tern-live');
    final home = '${tmp.path}/.tern';
    await Directory(home).create();
    final script = File('${tmp.path}/script.json');
    await script.writeAsString(
      jsonEncode({
        'turns': [
          {
            'text': 'Checking.',
            'tool_calls': [
              {
                'name': 'shell',
                'input': {'command': 'echo from-dart'},
              },
            ],
          },
          {'text': 'All done.'},
        ],
      }),
    );
    daemon = await Process.start(
      File(bin).absolute.path,
      ['daemon'],
      environment: {
        'PATH': Platform.environment['PATH'] ?? '',
        'HOME': tmp.path,
        'TERN_HOME': home,
        'TERN_PROVIDER': 'mock:${script.path}',
      },
      includeParentEnvironment: false,
    );
    client = TernClient(Endpoint.local({'TERN_HOME': home}));
    final end = DateTime.now().add(const Duration(seconds: 10));
    while (true) {
      try {
        await client.health();
        break;
      } on TernException {
        if (DateTime.now().isAfter(end)) rethrow;
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
    }
  });

  tearDownAll(() async {
    if (bin == null) return;
    client.close();
    daemon.kill();
    await daemon.exitCode;
    await tmp.delete(recursive: true);
  });

  test('streams a run, resyncs, and answers an approval', () async {
    final events = StreamQueue(client.events());
    // Let the SSE subscription register before the run starts.
    await Future<void>.delayed(const Duration(milliseconds: 200));
    final started = await client.send('hello from dart');

    final approval = await events.firstWhere<ApprovalRequested>();
    expect(approval.approval.summary, 'shell echo from-dart');
    expect((await client.pendingApprovals()).map((a) => a.id), [
      approval.approval.id,
    ]);
    expect((await client.activeRuns()).map((r) => r.runId), [started.runId]);

    await client.answer(approval.approval.id, Decision.once);
    final tool = await events.firstWhere<ToolFinished>();
    expect(tool.output, contains('from-dart'));
    final done = await events.firstWhere<RunFinished>();
    expect(done.status, 'completed');

    final session = await client.session(started.sessionId);
    expect(
      session.messages.last.content.whereType<TextBlock>().single.text,
      'All done.',
    );
    expect((await client.sessions()).single.id, started.sessionId);
    await events.cancel();
  }, skip: bin == null ? 'set TERN_BIN to run' : false);
}

/// Buffers a stream so tests can wait for the next event of a type.
class StreamQueue {
  StreamQueue(Stream<TernEvent> stream) {
    _sub = stream.listen(_buf.add);
  }

  final _buf = <TernEvent>[];
  late final StreamSubscription<TernEvent> _sub;

  Future<T> firstWhere<T extends TernEvent>() async {
    final end = DateTime.now().add(const Duration(seconds: 10));
    while (DateTime.now().isBefore(end)) {
      final i = _buf.indexWhere((e) => e is T);
      if (i >= 0) return _buf.removeAt(i) as T;
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    throw TimeoutException('no ${T.toString()} event');
  }

  Future<void> cancel() => _sub.cancel();
}
