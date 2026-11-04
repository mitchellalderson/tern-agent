// An agent's channel suggestion (channels-design C5): a card with
// "Create #slug" (moving the conversation and routines, each optional) and
// "Not now".
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/channel_items.dart';

final _t0 = DateTime(2026, 11, 5, 9);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final accepted = <({int id, bool move, List<String>? routines})>[];
  final declined = <int>[];

  @override
  Future<String> acceptSuggestion(
    int id, {
    bool moveConversation = true,
    List<String>? routines,
  }) async {
    accepted.add((id: id, move: moveConversation, routines: routines));
    return 'fin';
  }

  @override
  Future<void> declineSuggestion(int id) async => declined.add(id);

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      updatedAt: _t0,
    ),
    Channel(
      id: 'fin',
      slug: 'finances',
      title: 'Finances',
      kind: 'topic',
      updatedAt: _t0,
    ),
  ];

  @override
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [];

  @override
  Future<List<Schedule>> schedules() async => [];

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => [];

  @override
  Future<Usage> usage(String session) async => const Usage();
}

Post _card({String state = 'pending', String? channelId}) => Post(
  id: 7,
  channelId: 'home',
  sessionId: 'h1',
  authorKind: 'agent',
  authorId: 'tern',
  author: 'tern',
  kind: 'needs_you',
  text: 'Suggests a new channel, #finances: Invoices keep coming up.',
  suggestion: ChannelSuggestion(
    id: 3,
    slug: 'finances',
    title: 'Finances',
    why: 'Invoices keep coming up.',
    routines: const [(id: 'sch1', title: 'Sweep invoices every morning')],
    state: state,
    channelId: channelId,
    sessionId: 'h1',
  ),
  createdAt: _t0,
);

Future<(TernStore, _FakeClient)> _pump(WidgetTester tester, Post post) async {
  final client = _FakeClient();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0', features: {'channels'});
  store.channels = await client.channels();
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: PostCard(post: post, store: store),
      ),
    ),
  );
  return (store, client);
}

void main() {
  test('a suggestion decodes from a post', () {
    final p = Post.fromJson({
      'id': 7,
      'channel_id': 'home',
      'author_kind': 'agent',
      'author_id': 'tern',
      'author': 'tern',
      'kind': 'needs_you',
      'text': 'Suggests…',
      'created_at': 1,
      'suggestion': {
        'id': 3,
        'slug': 'finances',
        'title': 'Finances',
        'why': 'Invoices.',
        'routines': [
          {'id': 'sch1', 'title': 'Sweep'},
        ],
        'state': 'pending',
        'session_id': 'h1',
      },
    });
    expect(p.suggestion!.slug, 'finances');
    expect(p.suggestion!.routines.single.id, 'sch1');
    expect(p.suggestion!.pending, isTrue);
  });

  testWidgets('accepting can leave the routine where it is', (tester) async {
    final (store, client) = await _pump(tester, _card());
    expect(find.text('Create #finances'), findsOneWidget);
    expect(find.text('Move this conversation there'), findsOneWidget);
    await tester.tap(
      find.text('Move the routine "Sweep invoices every morning" there'),
    );
    await tester.pump();
    await tester.tap(find.text('Create #finances'));
    await tester.pump();
    await tester.pump();
    final a = client.accepted.single;
    expect((a.id, a.move), (3, true));
    expect(a.routines, isEmpty);
    expect(store.channel, 'fin', reason: 'opens the new channel');
  });

  testWidgets('not now declines', (tester) async {
    final (_, client) = await _pump(tester, _card());
    await tester.tap(find.text('Not now'));
    await tester.pump();
    expect(client.declined, [3]);
    expect(client.accepted, isEmpty);
  });

  testWidgets('a decided card says how', (tester) async {
    await _pump(tester, _card(state: 'accepted', channelId: 'fin'));
    expect(find.text('Create #finances'), findsNothing);
    expect(find.text('Go to #finances'), findsOneWidget);
  });
}
