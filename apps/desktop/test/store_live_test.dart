// TernStore against a real daemon with the mock provider and the demo
// script. Needs the tern binary; skipped without it:
//   TERN_BIN=../../target/debug/tern flutter test test/store_live_test.dart
import 'dart:async';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';

Future<void> until(bool Function() ok, String what) async {
  final end = DateTime.now().add(const Duration(seconds: 10));
  while (!ok()) {
    if (DateTime.now().isAfter(end)) fail('timed out waiting for $what');
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
}

void main() {
  final bin = Platform.environment['TERN_BIN'];

  test('send, stream, approve, finish; then a fresh store resyncs', () async {
    final tmp = await Directory.systemTemp.createTemp('tern-store');
    final home = '${tmp.path}/.tern';
    await Directory(home).create();
    final daemon = await Process.start(
      File(bin!).absolute.path,
      ['daemon'],
      environment: {
        'PATH': Platform.environment['PATH'] ?? '',
        'HOME': tmp.path,
        'TERN_HOME': home,
        'TERN_PROVIDER': 'mock:${File('tool/demo_script.json').absolute.path}',
      },
      includeParentEnvironment: false,
    );
    final endpoint = Endpoint.local({'TERN_HOME': home});
    final store = TernStore(TernClient(endpoint));
    addTearDown(() async {
      store.dispose();
      daemon.kill();
      await daemon.exitCode;
      await tmp.delete(recursive: true);
    });

    await until(() => File('$home/tern.sock').existsSync(), 'socket');
    await store.connect();
    expect(store.link, Link.online);

    await store.send('please approve this');
    expect(store.selected, isNotNull);
    await until(
      () => store.current!.items.whereType<ApprovalItem>().isNotEmpty,
      'approval card',
    );
    final card = store.current!.items.whereType<ApprovalItem>().single;
    expect(card.approval.summary, 'shell echo needs-approval');
    expect(store.pendingApprovals, 1);

    // A second app attaching now sees the run and the waiting approval.
    final other = TernStore(TernClient(endpoint));
    addTearDown(other.dispose);
    other.selected = store.selected;
    await other.connect();
    expect(other.current!.activeRun, isNotNull);
    expect(other.pendingApprovals, 1);

    await store.answer(card, Decision.once);
    await until(() => store.current!.activeRun == null, 'run to finish');
    final items = store.current!.items;
    expect(items.first, isA<UserItem>());
    expect(
      items.whereType<WorkItem>().single.steps.single.output,
      contains('needs-approval'),
    );
    expect(
      (items.last as AssistantItem).text,
      'Done. The command ran after you approved it.',
    );
    await until(() => store.sessions.isNotEmpty, 'session list');
    expect(store.sessions.single.title, 'please approve this');

    // Reopening rebuilds the same conversation from history.
    await store.open(store.selected!);
    expect(store.current!.items.map((i) => i.runtimeType), [
      UserItem,
      AssistantItem,
      WorkItem,
      AssistantItem,
    ]);
  }, skip: bin == null ? 'set TERN_BIN to run' : false);
}
