// The Tasks view (tooling spec §8.2) against a fake client.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/tasks.dart';

final taskNow = DateTime(2026, 11, 4, 9);

Json _t(
  String section,
  String kind,
  String id,
  String title, {
  String? detail,
  Duration? at,
  String? run,
  String? parent,
  String? session,
  String? channel,
  String? routine,
  bool tainted = false,
}) => {
  'section': section,
  'kind': kind,
  'id': id,
  'title': title,
  'detail': ?detail,
  if (at != null) 'at': taskNow.add(at).millisecondsSinceEpoch,
  'run_id': ?run,
  'parent_run_id': ?parent,
  'session_id': ?session,
  'channel_id': ?channel,
  'agent_id': 'tern',
  'routine_id': ?routine,
  'tainted': tainted,
};

final sampleTasks = Tasks.fromJson({
  'items': [
    _t(
      'needs_you',
      'approval',
      'a1',
      'write · shell: git push',
      run: 'r1',
      session: 's1',
      channel: 'c-dev',
    ),
    _t(
      'needs_you',
      'outbox',
      '3',
      'write · gmail_send to bob@example.com',
      detail: 'outbox #3, expires in 71h',
      at: const Duration(hours: -1),
      session: 's2',
      channel: 'home',
      tainted: true,
    ),
    _t(
      'running',
      'run',
      'r1',
      'Morning brief',
      at: const Duration(minutes: -2),
      run: 'r1',
      session: 's1',
      routine: 'brief',
    ),
    _t(
      'running',
      'run',
      'r2',
      'sub-agent research',
      run: 'r2',
      parent: 'r1',
      session: 's1-kid',
    ),
    _t(
      'queued',
      'events',
      'inbox',
      'Inbox sweep',
      detail: '2 events waiting',
      routine: 'inbox',
    ),
    _t(
      'scheduled',
      'routine',
      'brief',
      'Morning brief',
      detail: '0 7 * * *',
      at: const Duration(hours: 22),
      routine: 'brief',
    ),
    _t(
      'failed',
      'run',
      'r0',
      'Weekly report',
      detail: 'provider said no',
      at: const Duration(hours: -3),
      run: 'r0',
      session: 's0',
    ),
  ],
  'counts': {
    'needs_you': 2,
    'running': 2,
    'queued': 1,
    'scheduled': 1,
    'failed': 1,
  },
});

class FakeTasksClient extends TernClient {
  FakeTasksClient() : super(const Endpoint.unix('/nonexistent'));

  var calls = 0;

  @override
  Future<Tasks> tasks() async {
    calls++;
    return sampleTasks;
  }
}

Future<TernStore> pumpTasks(WidgetTester tester) async {
  final store = TernStore(FakeTasksClient());
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: TasksView(
          store: store,
          panelOpen: false,
          onTogglePanel: () {},
          refreshEvery: const Duration(hours: 1),
          now: () => taskNow,
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
  return store;
}

void main() {
  test('decodes tasks and says when', () {
    expect(sampleTasks.section('running').length, 2);
    expect(sampleTasks.counts['needs_you'], 2);
    expect(taskWhen(taskNow.add(const Duration(hours: 3)), taskNow), 'in 3h');
    expect(
      taskWhen(taskNow.subtract(const Duration(minutes: 5)), taskNow),
      '5m ago',
    );
  });

  testWidgets('shows every section with sub-agents under their parent', (
    tester,
  ) async {
    await pumpTasks(tester);
    for (final h in [
      'Needs you',
      'Running',
      'Queued',
      'Scheduled',
      'Failed recently',
    ]) {
      expect(find.text(h), findsOneWidget, reason: h);
    }
    expect(find.text('write · shell: git push'), findsOneWidget);
    expect(find.textContaining('provider said no'), findsOneWidget);
    expect(find.textContaining('in 22h'), findsOneWidget);
    // The sub-agent is indented under its parent.
    final parent = tester.getTopLeft(find.text('Morning brief').first);
    final kid = tester.getTopLeft(find.text('sub-agent research'));
    expect(kid.dx, greaterThan(parent.dx));
    expect(kid.dy, greaterThan(parent.dy));
  });

  testWidgets('each item opens where it lives', (tester) async {
    final store = await pumpTasks(tester);
    await tester.tap(find.text('Inbox sweep'));
    await tester.pump();
    expect(store.page, LibraryPage.routines);

    store.page = LibraryPage.tasks;
    await tester.tap(find.text('Weekly report'));
    await tester.pump();
    expect(store.thread, 's0');
  });

  testWidgets('a wait reads "Waiting: <note>" and can be cancelled', (
    tester,
  ) async {
    final client = _WaitsClient();
    final store = TernStore(client);
    await tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: Scaffold(
          body: TasksView(
            store: store,
            panelOpen: false,
            onTogglePanel: () {},
            refreshEvery: const Duration(hours: 1),
            now: () => taskNow,
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('Waiting: tell Mitch whether CI passed'), findsOneWidget);
    expect(find.textContaining('status == completed'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(client.cancelled, ['w1']);
    expect(find.textContaining('Waiting:'), findsNothing);
  });

  test('decodes a wait', () {
    final w = Wait.fromJson({
      'id': 'w1',
      'note': 'n',
      'agent_id': 'tern',
      'session_id': 's',
      'channel_id': 'home',
      'target': 'github.run_status',
      'until': 'status == completed',
      'expires_at': taskNow.millisecondsSinceEpoch,
    });
    expect((w.id, w.channelId, w.until), ('w1', 'home', 'status == completed'));
    expect(w.expiresAt, taskNow);
  });
}

class _WaitsClient extends TernClient {
  _WaitsClient() : super(const Endpoint.unix('/nonexistent'));

  final cancelled = <String>[];

  @override
  Future<Tasks> tasks() async => Tasks.fromJson({
    'items': [
      if (cancelled.isEmpty)
        _t(
          'scheduled',
          'wait',
          'w1',
          'Waiting: tell Mitch whether CI passed',
          detail: 'status == completed · checks github.run_status',
          at: const Duration(hours: 1),
          session: 's1',
        ),
    ],
    'counts': {'scheduled': cancelled.isEmpty ? 1 : 0},
  });

  @override
  Future<void> cancelWait(String id) async => cancelled.add(id);
}
