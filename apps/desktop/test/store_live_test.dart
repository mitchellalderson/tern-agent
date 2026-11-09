// TernStore against a real daemon with the mock provider and the demo
// script. Needs the tern binary; skipped without it:
//   TERN_BIN=../../target/debug/tern flutter test test/store_live_test.dart
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/notifier.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';

Future<void> until(bool Function() ok, String what) async {
  final end = DateTime.now().add(const Duration(seconds: 10));
  while (!ok()) {
    if (DateTime.now().isAfter(end)) fail('timed out waiting for $what');
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
}

/// A daemon in a fresh home with the mock provider on [script], and a
/// store connected to it. Torn down after the test.
Future<({TernStore store, FakeNotifier notes, Endpoint endpoint})> _daemon(
  String bin,
  String script, {
  String config = '',
}) async {
  final tmp = await Directory.systemTemp.createTemp('tern-store');
  final home = '${tmp.path}/.tern';
  await Directory(home).create();
  await File('$home/config.toml').writeAsString(config);
  final daemon = await Process.start(
    File(bin).absolute.path,
    ['daemon'],
    environment: {
      'PATH': Platform.environment['PATH'] ?? '',
      'HOME': tmp.path,
      'TERN_HOME': home,
      'TERN_PROVIDER': 'mock:${File(script).absolute.path}',
    },
    includeParentEnvironment: false,
  );
  final endpoint = Endpoint.local({'TERN_HOME': home});
  final notes = FakeNotifier();
  final store = TernStore(TernClient(endpoint), notifier: notes)
    ..appActive = false;
  addTearDown(() async {
    store.dispose();
    daemon.kill();
    await daemon.exitCode;
    await tmp.delete(recursive: true);
  });
  await until(() => File('$home/tern.sock').existsSync(), 'socket');
  return (store: store, notes: notes, endpoint: endpoint);
}

void main() {
  final bin = Platform.environment['TERN_BIN'];

  test('send, stream, approve, finish; then a fresh store resyncs', () async {
    final (:store, :notes, :endpoint) = await _daemon(
      bin!,
      'tool/demo_script.json',
    );
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
    // In the background: an approval notification and a badge.
    await until(() => notes.shown.isNotEmpty, 'notification');
    expect(notes.shown.single.approval, isTrue);
    expect(notes.badges.last, 1);

    // A second app attaching now sees the run and the waiting approval.
    final other = TernStore(TernClient(endpoint));
    addTearDown(other.dispose);
    other.selected = store.selected;
    await other.connect();
    expect(other.current!.activeRun, isNotNull);
    expect(other.pendingApprovals, 1);

    // Allow it from the notification's button.
    notes.onAction!(card.approval.id, NotificationAction.allow);
    await until(() => store.current!.activeRun == null, 'run to finish');
    await until(() => notes.shown.length == 2, 'finished notification');
    expect(
      notes.shown.last.body,
      'Done. The command ran after you approved it.',
    );
    expect(notes.badges.last, 0);
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

  test('sub-agents: one card, their approval in this conversation', () async {
    final (:store, :notes, endpoint: _) = await _daemon(
      bin!,
      'test/fixtures/subagents_script.json',
      config: '[subagents]\nenabled = true\n',
    );
    await store.connect();
    await store.send('delegate two things');
    final session = store.selected!;

    // Sub-agent a's shell call (a write; a plain `ls` is a read and no longer
    // asks) asks here, labelled with its name.
    await until(
      () => store.current!.items.whereType<ApprovalItem>().isNotEmpty,
      'approval from a sub-agent',
    );
    final ask = store.current!.items.whereType<ApprovalItem>().single;
    expect(ask.approval.agentLabel, 'a');
    expect(ask.approval.rootSessionId, session);
    await until(() => notes.shown.isNotEmpty, 'notification');
    expect(notes.shown.first.title, 'a wants to run shell');

    // The card fills in as the spawn events arrive: the approval can land
    // before sub-agent b's, so wait for the whole card, not its first draw.
    AgentsItem? cardNow() =>
        store.current!.items.whereType<AgentsItem>().singleOrNull;
    await until(() {
      final c = cardNow();
      return c != null &&
          c.children.length == 2 &&
          c.children.every((k) => k.sessionId != null);
    }, 'both sub-agents on the card');
    final card = cardNow()!;
    expect(card.children.map((c) => c.label), ['a', 'b']);
    // No tool lines for the agent_* calls.
    expect(store.current!.items.whereType<WorkItem>(), isEmpty);

    await store.answer(ask, Decision.once);
    // The run's end and the card's final state arrive as separate events.
    await until(
      () =>
          store.current!.activeRun == null &&
          card.children.every(
            (c) => c.status == 'completed' && c.result != null,
          ) &&
          store.current!.items.last is AssistantItem &&
          ask.outcome != null,
      'run to finish with the card settled',
    );
    expect(card.children.map((c) => c.status), ['completed', 'completed']);
    expect(card.children.map((c) => c.result), [
      'report from a',
      'report from b',
    ]);
    expect((store.current!.items.last as AssistantItem).text, 'Both reported.');
    expect(ask.outcome, isNotNull);

    // Sub-agent sessions stay out of the sidebar.
    await until(() => store.sessions.isNotEmpty, 'session list');
    expect(store.sessions.map((s) => s.id), [session]);

    // Reopening rebuilds the card, and the daemon fills in the details.
    await store.open(session);
    final again = store.current!.items.whereType<AgentsItem>().single;
    expect(again.children.map((c) => c.status), ['completed', 'completed']);
    expect(again.children.first.sessionId, card.children.first.sessionId);
    await store.loadChild(again.children.first.sessionId!);
    expect(
      store.transcripts[again.children.first.sessionId]!.items
          .whereType<AssistantItem>()
          .last
          .text,
      'report from a',
    );
  }, skip: bin == null ? 'set TERN_BIN to run' : false);
}

class FakeNotifier implements Notifier {
  final shown = <({String id, String title, String body, bool approval})>[];
  final badges = <int>[];

  @override
  void Function(String id, NotificationAction action)? onAction;

  @override
  Future<void> show({
    required String id,
    required String title,
    required String body,
    bool approval = false,
  }) async => shown.add((id: id, title: title, body: body, approval: approval));

  @override
  Future<void> badge(int count) async => badges.add(count);
}
