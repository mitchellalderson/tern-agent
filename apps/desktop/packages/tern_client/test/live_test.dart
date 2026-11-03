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
  late int port;

  setUpAll(() async {
    if (bin == null) return;
    tmp = await Directory.systemTemp.createTemp('tern-live');
    final home = '${tmp.path}/.tern';
    await Directory(home).create();
    final free = await ServerSocket.bind('127.0.0.1', 0);
    port = free.port;
    await free.close();
    await File(
      '$home/config.toml',
    ).writeAsString('[api]\nlisten = ["unix", "tcp://127.0.0.1:$port"]\n');
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
    expect(tool.sessionId, started.sessionId);
    final done = await events.firstWhere<RunFinished>();
    expect(done.status, 'completed');

    final session = await client.session(started.sessionId);
    expect(
      session.messages.last.content.whereType<TextBlock>().single.text,
      'All done.',
    );
    expect((await client.sessions()).single.id, started.sessionId);
    expect(session.messages.last.runId, started.runId);
    final steps = await client.steps(started.runId);
    expect(steps.map((s) => s.kind), containsAll(['llm', 'approval', 'tool']));
    expect((await client.runs(started.sessionId)).single.status, 'completed');
    await events.cancel();
  }, skip: bin == null ? 'set TERN_BIN to run' : false);

  test('routines: preview, create, run now, edit, delete', () async {
    final preview = await client.previewCron('0 8 * * mon-fri');
    expect(preview.next, hasLength(3));
    await expectLater(
      client.previewCron('nope'),
      throwsA(isA<TernException>().having((e) => e.status, 'status', 400)),
    );

    final s = await client.createSchedule(
      cron: '0 8 * * 1-5',
      prompt: 'brief me',
    );
    expect(s.enabled, isTrue);
    expect(s.nextRunAt, isNotNull);
    final started = await client.runSchedule(s.id);
    expect(started.sessionId, s.sessionId);

    final off = await client.updateSchedule(s.id, enabled: false);
    expect(off.nextRunAt, isNull);
    expect((await client.schedules()).single.enabled, isFalse);
    await client.deleteSchedule(s.id);
    expect(await client.schedules(), isEmpty);
  }, skip: bin == null ? 'set TERN_BIN to run' : false);

  test('secrets: set, list without values, remove', () async {
    final before = await client.secrets();
    expect(before.where((s) => s.name == 'DART_TEST_TOKEN'), isEmpty);

    final set = await client.setSecret('DART_TEST_TOKEN', 'dart-secret-value');
    expect(set.inFile, isTrue);
    final file = File('${tmp.path}/.tern/secrets.toml');
    expect(await file.readAsString(), contains('dart-secret-value'));
    expect(
      jsonEncode((await client.secrets()).map((s) => s.name).toList()),
      contains('DART_TEST_TOKEN'),
    );

    await expectLater(
      client.setSecret('bad-name', 'x'),
      throwsA(isA<TernException>().having((e) => e.status, 'status', 400)),
    );
    expect(await client.removeSecret('DART_TEST_TOKEN'), isTrue);
    expect(await client.removeSecret('DART_TEST_TOKEN'), isFalse);
  }, skip: bin == null ? 'set TERN_BIN to run' : false);

  test(
    'pairs over TLS with a pinned certificate, then is signed out',
    () async {
      final offer = await client.offerPairing(scope: 'chat', name: 'dart test');
      expect(offer.link, contains('127.0.0.1%3A$port'));
      final paired = await TernClient.pair(
        PairingLink.parse(offer.link!),
        deviceName: 'ignored: the offer named it',
        platform: 'test',
      );
      expect(paired.scope, 'chat');
      final remote = TernClient(paired.endpoint);
      addTearDown(remote.close);

      final health = await remote.health();
      expect(health.deployment, 'server');
      expect(health.scope, 'chat');
      expect(await remote.sessions(), isNotEmpty);
      await expectLater(
        remote.devices(),
        throwsA(isA<TernException>().having((e) => e.status, 'status', 403)),
      );

      final device = (await client.devices()).firstWhere(
        (d) => d.id == paired.deviceId,
      );
      expect(device.name, 'dart test');
      await client.revokeDevice(paired.deviceId);
      await expectLater(remote.sessions(), throwsA(isA<SignedOutException>()));
    },
    skip: bin == null ? 'set TERN_BIN to run' : false,
  );
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
