// "New since you left": a divider above what arrived while the reader was
// away, and a long catch-up opens there instead of at the bottom.
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/feed.dart';
import 'package:tern_desktop/ui/home.dart';

final _t0 = DateTime(2026, 11, 3, 9);

Post _post(int id) => Post(
  id: id,
  channelId: 'fin',
  sessionId: 'main',
  runId: 'r$id',
  authorKind: 'agent',
  authorId: 'tern',
  author: 'tern',
  kind: 'answer',
  text: 'Update number $id',
  createdAt: _t0.add(Duration(minutes: id)),
);

class _Client extends TernClient {
  _Client(this.count) : super(const Endpoint.unix('/nonexistent'));
  final int count;

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
      lastPostId: count,
      updatedAt: _t0,
    ),
  ];

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => [for (var i = count; i > 0; i--) _post(i)].take(limit).toList();

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async => const StreamPage(items: []);

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<SessionDetail> session(String id, {int? limit, int? before}) async =>
      SessionDetail(id: id, messages: const []);
}

Future<void> _open(WidgetTester tester, int seen, {int total = 60}) async {
  final client = _Client(total);
  final store = TernStore(client, lastSeen: {'fin': seen})
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0.2.0', features: {'channels'})
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  store.channels = await client.channels();
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
  await store.openChannelView('fin');
  for (var i = 0; i < 12; i++) {
    await tester.pump(const Duration(milliseconds: 50));
  }
}

void main() {
  test('catchUpAt finds the first new post and counts them', () {
    expect(catchUpAt(null, [1, 2, 3]), isNull);
    expect(catchUpAt(0, [1, 2, 3]), isNull);
    expect(catchUpAt(3, [1, 2, 3]), isNull);
    final at = catchUpAt(2, [1, null, 2, null, 3, 4])!;
    expect(at.index, 4);
    expect(at.count, 2);
  });

  testWidgets('a long catch-up opens at the divider, not the bottom', (
    tester,
  ) async {
    await _open(tester, 10);
    expect(find.text('New since you left'), findsOneWidget);
    expect(find.text('Update number 11'), findsOneWidget);
    expect(find.text('Update number 60'), findsNothing);
    expect(find.text('Update number 10'), findsOneWidget);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('a short catch-up keeps the bottom, with the divider above', (
    tester,
  ) async {
    await _open(tester, 58);
    expect(find.text('New since you left'), findsOneWidget);
    expect(find.text('Update number 60'), findsOneWidget);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('nothing new: no divider', (tester) async {
    await _open(tester, 60);
    expect(find.text('New since you left'), findsNothing);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('never read: no divider', (tester) async {
    await _open(tester, 0);
    expect(find.text('New since you left'), findsNothing);
    debugDefaultTargetPlatformOverride = null;
  });
}
