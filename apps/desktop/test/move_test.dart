// Moving conversations into channels (DECISIONS.md "T2 moves").
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/channel_items.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/thread_pane.dart';

final _t0 = DateTime(2026, 11, 4, 9);

StreamMessage _m(String s, int id, String role, String text) => StreamMessage(
  sessionId: s,
  messageId: id,
  message: SessionMessage(
    role: role,
    content: [TextBlock(text)],
    summary: false,
  ),
  createdAt: _t0.add(Duration(minutes: id)),
);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final moves = <({List<String> sessions, String? channel, String? title})>[];

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      mainSessionId: 'h2',
      updatedAt: _t0,
    ),
    Channel(
      id: 'launch',
      slug: 'launch',
      title: 'Launch',
      kind: 'topic',
      updatedAt: _t0,
    ),
    if (moves.any((m) => m.title != null))
      Channel(
        id: 'trip',
        slug: 'trip',
        title: 'Trip',
        kind: 'topic',
        updatedAt: _t0,
      ),
  ];

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async => StreamPage(
    items: [
      _m('h2', 5, 'assistant', 'Sure.'),
      _m('h2', 4, 'user', 'Plan the trip'),
      StreamCut(
        sessionId: 'h2',
        prevSessionId: 'h1',
        reason: 'new_topic',
        createdAt: _t0.add(const Duration(minutes: 3)),
      ),
      StreamPost(
        Post(
          id: 9,
          channelId: 'home',
          sessionId: 'moved1',
          authorKind: 'user',
          authorId: 'owner',
          author: 'you',
          kind: 'moved',
          text: 'Moved to #launch: Book the venue',
          pointsTo: 8,
          pointsToChannel: 'launch',
          tainted: true,
          createdAt: _t0.add(const Duration(minutes: 2)),
        ),
      ),
      _m('h1', 1, 'user', 'Weekly sync'),
    ],
  );

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => [];

  @override
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [
        SessionItem(
          id: 'h2',
          title: 'Plan the trip',
          source: 'api',
          messages: 2,
          runs: 1,
          updatedAt: _t0,
        ),
        SessionItem(
          id: 'h1',
          title: 'Weekly sync',
          source: 'api',
          messages: 2,
          runs: 1,
          updatedAt: _t0,
        ),
        SessionItem(
          id: 'side',
          title: 'Caterer?',
          source: 'api',
          messages: 2,
          runs: 1,
          updatedAt: _t0,
          branchedFrom: 'message:1',
        ),
      ];

  @override
  Future<SessionDetail> session(String id, {int? limit, int? before}) async =>
      SessionDetail(id: id, messages: const []);

  @override
  Future<MoveResult> moveToChannel(
    List<String> sessions, {
    String? channel,
    String? newChannelTitle,
  }) async {
    moves.add((sessions: sessions, channel: channel, title: newChannelTitle));
    final target = (await channels()).firstWhere(
      (c) => c.id == (channel ?? 'trip'),
    );
    return MoveResult(
      channel: target,
      moved: [
        for (final s in sessions) (sessionId: s, from: 'home', fresh: null),
      ],
    );
  }
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

/// Taps in the title bar wait out its double-click recognizer.
Future<void> _settle(WidgetTester tester) async {
  await tester.pump(const Duration(milliseconds: 500));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('the marker where a topic was goes to its channel', (
    tester,
  ) async {
    final (store, _) = await _store();
    await _pump(tester, store);
    expect(find.byType(MovedMarker), findsOneWidget);
    expect(find.text('Moved to #launch: Book the venue'), findsOneWidget);
    await tester.tap(find.text('Moved to #launch: Book the venue'));
    await tester.pump();
    await tester.pump();
    expect(store.channel, 'launch');
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('Home\'s current topic moves into a new channel', (tester) async {
    final (store, client) = await _store();
    await _pump(tester, store);
    await tester.tap(find.byTooltip('Move this topic to a channel').first);
    await _settle(tester);
    expect(find.text('Move to channel'), findsOneWidget);
    expect(find.text('#launch'), findsOneWidget);
    await tester.tap(find.text('New channel'));
    await tester.pump();
    await tester.enterText(
      find.widgetWithText(TextField, 'Name, e.g. Launch'),
      'Trip',
    );
    await tester.pump();
    expect(find.text('#trip'), findsOneWidget);
    await tester.tap(find.widgetWithText(FilledButton, 'Move'));
    await _settle(tester);
    expect(client.moves.single.sessions, ['h2']);
    expect(
      (client.moves.single.channel, client.moves.single.title),
      (null, 'Trip'),
    );
    expect(store.channel, 'trip');
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('History gathers several into one channel', (tester) async {
    final (store, client) = await _store();
    await _pump(tester, store);
    await tester.tap(find.text('History'));
    await _settle(tester);
    await tester.tap(find.byType(Checkbox).at(1));
    await tester.tap(find.byType(Checkbox).at(2));
    await tester.pump();
    expect(find.text('2 picked'), findsOneWidget);
    await tester.tap(find.text('Move to channel…'));
    await _settle(tester);
    // #launch is picked first.
    await tester.tap(find.widgetWithText(FilledButton, 'Move'));
    await _settle(tester);
    expect(client.moves.single.sessions, ['h1', 'side']);
    expect(
      (client.moves.single.channel, client.moves.single.title),
      ('launch', null),
    );
    expect(store.channel, 'launch');
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('a thread open beside the stream can move too', (tester) async {
    final (store, _) = await _store();
    await store.openThread('side');
    await _pump(tester, store);
    expect(
      find.descendant(
        of: find.byType(ThreadPane),
        matching: find.byTooltip('Move to a channel'),
      ),
      findsOneWidget,
    );
    // And dividers offer it for the topic they start.
    expect(find.byTooltip('Move this topic to a channel'), findsNWidgets(2));
    debugDefaultTargetPlatformOverride = null;
  });
}
