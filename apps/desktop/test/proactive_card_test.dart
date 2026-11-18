// Proactive help: a dismissible card in the agent's DM, a presence report,
// the learned "Always allow this", and the grants list in Settings.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/channel_items.dart';
import 'package:tern_desktop/ui/grants.dart';

final _t0 = DateTime(2026, 11, 18, 9);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final accepted = <int>[];
  final dismissed = <int>[];
  var presence = 0;
  var grantList = const [
    Grant(
      tool: 'shell',
      pattern: 'git status *',
      note: 'git status *',
    ),
  ];
  final revoked = <String>[];

  @override
  Future<void> acceptCard(int id) async => accepted.add(id);

  @override
  Future<void> dismissCard(int id) async => dismissed.add(id);

  @override
  Future<bool> reportPresence() async {
    presence++;
    return false;
  }

  @override
  Future<List<Grant>> grants() async => grantList;

  @override
  Future<void> revokeGrant(Grant g) async {
    revoked.add(g.pattern);
    grantList = const [];
  }
}

Post _post({String? acceptLabel, String state = 'pending'}) => Post(
  id: 9,
  channelId: 'home',
  authorKind: 'agent',
  authorId: 'tern',
  author: 'tern',
  kind: 'needs_you',
  text: 'Make this a routine?\nYou have asked for this a few times.',
  card: ProactiveCard(
    id: 4,
    kind: 'routine_suggestion',
    title: 'Make this a routine?',
    acceptLabel: acceptLabel,
    state: state,
  ),
  createdAt: _t0,
);

Future<_FakeClient> _pump(WidgetTester tester, Post post) async {
  final client = _FakeClient();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0', features: {'channels'});
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(body: PostCard(post: post, store: store)),
    ),
  );
  return client;
}

void main() {
  test('a card decodes from a post', () {
    final p = Post.fromJson({
      'id': 9,
      'channel_id': 'home',
      'author_kind': 'agent',
      'author_id': 'tern',
      'author': 'tern',
      'kind': 'needs_you',
      'text': 'x',
      'created_at': 1,
      'card': {
        'id': 4,
        'kind': 'routine_fix',
        'title': 'Routine failed',
        'body': 'Use the Inbox label.',
        'accept_label': 'Apply fix',
        'state': 'pending',
        'agent_id': 'tern',
        'created_at': 1,
      },
    });
    expect(p.card!.acceptLabel, 'Apply fix');
    expect(p.card!.pending, isTrue);
    expect(p.resolved().card!.id, 4);
  });

  test('a grant offer says whether "always" is offered', () {
    expect(GrantOffer.fromJson({'key': 'k', 'label': 'l'}).always, isTrue);
    expect(
      GrantOffer.fromJson({'key': 'k', 'label': 'l', 'always': false}).always,
      isFalse,
    );
  });

  testWidgets('a card can be acted on or dismissed', (tester) async {
    final client = await _pump(tester, _post(acceptLabel: 'Set up routine'));
    await tester.tap(find.text('Set up routine'));
    await tester.pump();
    await tester.pump();
    expect(client.accepted, [4]);
    expect(find.text('Done ✓'), findsOneWidget);
  });

  testWidgets('a card with nothing to act on only dismisses', (tester) async {
    final client = await _pump(tester, _post());
    expect(find.text('Set up routine'), findsNothing);
    await tester.tap(find.text('Dismiss'));
    await tester.pump();
    await tester.pump();
    expect(client.dismissed, [4]);
    expect(find.text('Dismissed'), findsOneWidget);
  });

  testWidgets('a decided card shows how it ended', (tester) async {
    await _pump(tester, _post(state: 'dismissed'));
    expect(find.text('Dismiss'), findsNothing);
    expect(find.text('Dismissed'), findsOneWidget);
  });

  test('the store reports presence at most once a minute', () async {
    final client = _FakeClient();
    final store = TernStore(client);
    await store.reportPresence();
    await store.reportPresence();
    expect(client.presence, 1);
  });

  testWidgets('grants are listed and can be revoked', (tester) async {
    final client = _FakeClient();
    final store = TernStore(client);
    await tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: Scaffold(body: GrantsSection(store: store)),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('shell'), findsOneWidget);
    await tester.tap(find.text('Revoke'));
    await tester.pumpAndSettle();
    expect(client.revoked, ['git status *']);
    expect(find.textContaining('Nothing is always allowed'), findsOneWidget);
  });
}
