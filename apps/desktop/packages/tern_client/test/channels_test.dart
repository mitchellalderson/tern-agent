import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

void main() {
  test('a stream page decodes every kind of item', () {
    final page = StreamPage.fromJson(
      jsonDecode(File('test/fixtures/stream_page.json').readAsStringSync())
          as Json,
    );
    expect(page.next, '1762185600000.2.3');
    final [reply, queued, cut, post] = page.items;
    expect((reply as StreamMessage).message.role, 'assistant');
    expect(reply.messageId, 14);
    expect((queued as StreamMessage).queued, isTrue);
    final c = cut as StreamCut;
    expect(
      (c.reason, c.prevSessionId),
      ('new_topic', '019a0b1c2d3e7f00a1b2c3d4e5f60718'),
    );
    expect(c.carry, startsWith('[Continuing from your previous session'));
    final p = (post as StreamPost).post;
    expect(
      (p.authorKind, p.authorId, p.author),
      ('routine', 'routine:sch_0a1b2c3d4e5f', 'Morning brief'),
    );
    expect(p.mine, isFalse);
  });

  test('channels, sends and channel events decode', () {
    final home = Channel.fromJson({
      'id': 'home',
      'slug': 'home',
      'title': 'Home',
      'kind': 'main',
      'main_session_id': 's1',
      'needs_you': 2,
      'active_runs': 1,
      'updated_at': 1762185600000,
    });
    expect((home.isHome, home.needsYou, home.archived), (true, 2, false));
    final sent = Sent.fromJson({
      'post': null,
      'run_id': 'r',
      'session_id': 's2',
      'queued': false,
      'new_segment': true,
    });
    expect((sent.post, sent.newSegment), (null, true));

    final created = TernEvent.fromJson({
      'post_created': {
        'post': {
          'id': 9,
          'channel_id': 'c',
          'author_kind': 'user',
          'author_id': 'owner',
          'author': 'you',
          'kind': 'message',
          'text': 'hi',
          'created_at': 1,
        },
      },
    });
    expect((created as PostCreated).post.mine, isTrue);
    expect(
      TernEvent.fromJson({'channels_changed': <String, dynamic>{}}),
      isA<ChannelsChanged>(),
    );
    final started = TernEvent.fromJson({
      'type': 'run_started',
      'run_id': 'r',
      'session_id': 's',
      'channel_id': 'c',
    });
    expect((started as RunStarted).channelId, 'c');
  });

  test("an outbox item's needs-you post names it and stays so resolved", () {
    final p = Post.fromJson(
      jsonDecode(
            '{"id":5,"channel_id":"home","author_kind":"agent","author_id":"tern",'
            '"author":"tern","kind":"pointer","text":"Waiting","points_to":4,'
            '"outbox_id":12,"created_at":1762185600000}',
          )
          as Json,
    );
    expect(p.outboxId, 12);
    expect(p.needsYou, isTrue);
    final done = p.resolved();
    expect((done.outboxId, done.needsYou), (12, false));
  });

  test('a waiting approval or secret request names it on its post', () {
    Post parse(String extra) => Post.fromJson(
      jsonDecode(
            '{"id":7,"channel_id":"c","author_kind":"agent","author_id":"tern",'
            '"author":"tern","kind":"needs_you","text":"Needs your approval",'
            '$extra"created_at":1762185600000}',
          )
          as Json,
    );
    final a = parse('"approval_id":"ap1",');
    expect((a.approvalId, a.secretRequestId, a.isRequest), ('ap1', null, true));
    expect(a.resolved().approvalId, 'ap1');
    final s = parse('"secret_request_id":"sr1",');
    expect((s.secretRequestId, s.isRequest), ('sr1', true));
    expect(parse('').isRequest, isFalse);
  });

  test('sessions carry their place in a stream', () {
    final s = SessionItem.fromJson({
      'id': 's2',
      'source': 'api',
      'messages': 2,
      'runs': 1,
      'updated_at': 1,
      'channel_id': 'home',
      'prev_session_id': 's1',
      'segment_reason': 'idle',
    });
    expect(
      (s.channelId, s.prevSessionId, s.segmentReason, s.branchedFrom),
      ('home', 's1', 'idle', null),
    );
  });
}
