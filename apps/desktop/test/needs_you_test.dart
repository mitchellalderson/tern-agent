// C4 in the app: member lines, needs-you posts and their pointers in Home
// (docs/channels-design.md §2.3, §5.2).
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/home.dart';

final _t0 = DateTime(2026, 11, 4, 9);

const _ask = 7;

Post _p(
  int id,
  String text, {
  String kind = 'update',
  String channel = 'fin',
  int? pointsTo,
  String author = 'tern',
}) => Post(
  id: id,
  channelId: channel,
  sessionId: channel == 'home' ? null : 'main',
  authorKind: author.contains('/') ? 'subagent' : 'agent',
  authorId: author,
  author: author,
  kind: kind,
  text: text,
  pointsTo: pointsTo,
  pointsToChannel: pointsTo == null ? null : 'fin',
  createdAt: _t0.add(Duration(minutes: id)),
);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final resolved = <int>[];

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      mainSessionId: 'h1',
      updatedAt: _t0,
    ),
    Channel(
      id: 'fin',
      slug: 'finances',
      title: 'Finances',
      kind: 'topic',
      mainSessionId: 'main',
      needsYou: resolved.isEmpty ? 1 : 0,
      updatedAt: _t0,
    ),
  ];

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => [
    _p(_ask, 'Should we sign with Acme?', kind: 'needs_you'),
    _p(6, 'Halfway: 40 sources read', author: 'tern/loud'),
  ];

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async => StreamPage(
    items: [
      StreamPost(
        _p(
          9,
          'Should we sign with Acme?',
          kind: 'pointer',
          channel: 'home',
          pointsTo: _ask,
        ),
      ),
    ],
  );

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<void> resolvePost(int id) async => resolved.add(id);
}

Future<(TernStore, _FakeClient)> _store() async {
  final client = _FakeClient();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0.2.0', features: {'channels'})
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  store.channels = await client.channels();
  await store.openHome();
  return (store, client);
}

Future<void> _pump(WidgetTester tester, TernStore store) async {
  tester.view.physicalSize = const Size(1500, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Home(store: store),
    ),
  );
  await tester.pump();
}

TernEvent _ev(Map<String, dynamic> j) =>
    TernEvent.fromJson({'session_id': 'x', 'channel_id': 'fin', ...j});

void main() {
  test('member lines follow run events, sub-agents by label', () async {
    final (store, _) = await _store();
    store.onEventForTest(
      _ev({'type': 'run_started', 'run_id': 'p', 'session_id': 'main'}),
    );
    store.onEventForTest(
      _ev({
        'type': 'subagent_spawned',
        'run_id': 'p',
        'call_id': 'c',
        'child_run_id': 'k',
        'child_session_id': 'ks',
        'label': 'loud',
        'task': 't',
        'model': 'm',
        'status': 'running',
      }),
    );
    store.onEventForTest(
      _ev({
        'type': 'run_started',
        'run_id': 'k',
        'session_id': 'ks',
        'parent_run_id': 'p',
      }),
    );
    store.onEventForTest(
      _ev({
        'type': 'tool_started',
        'run_id': 'k',
        'session_id': 'ks',
        'parent_run_id': 'p',
        'id': 't1',
        'name': 'http_fetch',
        'summary': 'http_fetch x',
      }),
    );
    final lines = store.liveIn('fin');
    expect(
      lines.map((l) => (l.author, l.doing)),
      contains(('tern/loud', 'Running http_fetch')),
    );
    expect(lines.map((l) => l.author), contains('tern'));
    expect(store.liveIn('home'), isEmpty);
    store.onEventForTest(
      _ev({
        'type': 'run_finished',
        'run_id': 'k',
        'session_id': 'ks',
        'status': 'completed',
      }),
    );
    expect(store.liveIn('fin').map((l) => l.author), ['tern']);
  });

  testWidgets('a pointer in Home goes to the channel; Done closes both', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _pump(tester, store);
    expect(find.text('needs you · #finances'), findsOneWidget);
    expect(find.text('Go to #finances'), findsOneWidget);

    await tester.tap(find.text('Go to #finances'));
    await tester.pump();
    await tester.pump();
    expect(store.channel, 'fin');
    expect(find.text('needs you'), findsOneWidget);
    expect(find.text('tern/loud'), findsOneWidget);

    await tester.tap(find.text('Done'));
    await tester.pump();
    await tester.pump();
    expect(client.resolved, [_ask]);
    expect(find.text('needs you'), findsNothing);
    expect(find.text('Done ✓'), findsOneWidget);
    // Its pointer in Home closed too.
    final pointer = store.transcripts[TernStore.homeKey]!.items
        .whereType<PostItem>()
        .single
        .post;
    expect(pointer.needsYou, isFalse);
    debugDefaultTargetPlatformOverride = null;
  });
}
