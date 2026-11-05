// A long conversation or channel loads a page at a time: the newest first,
// older ones on request, and a resync re-reads only a bounded amount.
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';

final _t0 = DateTime(2026, 11, 3, 9);

class _Client extends TernClient {
  _Client(this.messages, this.postCount)
    : super(const Endpoint.unix('/nonexistent'));

  /// A session's messages, oldest first.
  final List<SessionMessage> messages;
  final int postCount;
  final sessionCalls = <({int? limit, int? before})>[];
  final postCalls = <({int? before, int limit})>[];

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'fin',
      slug: 'finances',
      title: 'Finances',
      kind: 'topic',
      mainSessionId: 'main',
      updatedAt: _t0,
    ),
  ];

  @override
  Future<SessionDetail> session(String id, {int? limit, int? before}) async {
    sessionCalls.add((limit: limit, before: before));
    final end = before == null ? messages.length : before.clamp(0, 1 << 30);
    final start = limit == null ? 0 : (end - limit).clamp(0, end);
    return SessionDetail(
      id: id,
      messages: messages.sublist(start, end),
      total: messages.length,
      offset: start,
    );
  }

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async {
    postCalls.add((before: before, limit: limit));
    // Ids 1..postCount; newest first, those below `before`.
    final top = before == null ? postCount : before - 1;
    return [
      for (var id = top; id > 0 && id > top - limit; id--)
        Post(
          id: id,
          channelId: 'fin',
          authorKind: 'agent',
          authorId: 'tern',
          author: 'tern',
          kind: 'message',
          text: 'post $id',
          createdAt: _t0,
        ),
    ];
  }

  @override
  Future<Usage> usage(String session) async => const Usage();
}

SessionMessage _m(int i) => SessionMessage(
  role: i.isEven ? 'user' : 'assistant',
  runId: 'r',
  content: [TextBlock('message $i')],
  summary: false,
);

TernStore _store(_Client c) => TernStore(c)
  ..link = Link.online
  ..health = const Health(pid: 1, version: '0.2.0', features: {'channels'})
  ..info = const Info(defaultModel: 'mock:m', tools: []);

String _text(Item i) => switch (i) {
  UserItem(:final text) => text,
  AssistantItem(:final text) => text,
  _ => '',
};

void main() {
  test('a thread opens on its newest page and pages back', () async {
    final client = _Client([for (var i = 0; i < 450; i++) _m(i)], 0);
    final store = _store(client);
    await store.openThread('s1');
    expect(client.sessionCalls.single, (limit: 200, before: null));
    var items = store.threadTranscript!.items;
    expect(items, hasLength(200));
    expect(_text(items.first), 'message 250');
    expect(store.threadHasEarlier, isTrue);
    expect(store.threadEarlierCount, 250);

    await store.loadOlderThread();
    expect(client.sessionCalls.last, (limit: 200, before: 250));
    items = store.threadTranscript!.items;
    expect(items, hasLength(400));
    expect(_text(items.first), 'message 50');
    expect(_text(items.last), 'message 449');

    await store.loadOlderThread();
    expect(store.threadTranscript!.items, hasLength(450));
    expect(store.threadHasEarlier, isFalse);
    // Nothing left: no further request.
    final calls = client.sessionCalls.length;
    await store.loadOlderThread();
    expect(client.sessionCalls, hasLength(calls));
  });

  test('a short thread is read whole, with no earlier to show', () async {
    final store = _store(_Client([for (var i = 0; i < 30; i++) _m(i)], 0));
    await store.openThread('s1');
    expect(store.threadTranscript!.items, hasLength(30));
    expect(store.threadHasEarlier, isFalse);
  });

  test('a channel pages its posts back by id', () async {
    final client = _Client(const [], 250);
    final store = _store(client);
    await store.openChannelView('fin');
    expect(client.postCalls.single, (before: null, limit: 100));
    expect(store.posts['fin'], hasLength(100));
    expect(store.posts['fin']!.last.id, 250);
    expect(store.postsMore['fin'], isTrue);

    await store.loadOlderPosts('fin');
    expect(client.postCalls.last, (before: 151, limit: 100));
    expect(store.posts['fin'], hasLength(200));
    expect(store.posts['fin']!.first.id, 51);
    expect(store.postsMore['fin'], isTrue);

    await store.loadOlderPosts('fin');
    expect(store.posts['fin'], hasLength(250));
    expect(store.postsMore['fin'], isFalse);
  });

  test(
    'a resync re-reads what was paged in, but never more than the cap',
    () async {
      final client = _Client(const [], 2000);
      final store = _store(client);
      await store.openChannelView('fin');
      for (var i = 0; i < 8; i++) {
        await store.loadOlderPosts('fin');
      }
      expect(store.posts['fin'], hasLength(900));
      await store.openChannelView('fin');
      expect(client.postCalls.last.limit, TernStore.historyCap);
      expect(store.posts['fin'], hasLength(TernStore.historyCap));
    },
  );
}
