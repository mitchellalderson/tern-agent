import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

const _conn =
    '{"id":"c1","kind":"connector","target":"gmail","slug":"work",'
    '"label":"Work","account":"me@x.com","auth":{"method":"oauth","x":1},'
    '"is_default":true,"status":"needs_reauth","last_error":"expired",'
    '"owner_member":"m","created_at":1000,"last_used_at":null,'
    '"title":"gmail · work (me@x.com)"}';

void main() {
  test('decodes a choose card', () {
    final e = TernEvent.fromJson(
      jsonDecode(
            '{"auth_prompt":{"id":"a1","run_id":"","session_id":"",'
            '"call_id":"t1","card":{"kind":"choose","title":"Connect Notion",'
            '"options":[{"id":"oauth","label":"Sign in","detail":"d",'
            '"recommended":true},{"id":"key","label":"Key","detail":"",'
            '"recommended":false}],"notes":["n"]}}}',
          )
          as Json,
    );
    final p = (e as AuthPromptOpened).prompt;
    expect(p.id, 'a1');
    expect(p.callId, 't1');
    expect(p.card.choose, isTrue);
    expect(p.card.options.first.recommended, isTrue);
    expect(p.card.notes, ['n']);
  });

  test('decodes sign-in cards, browser and device', () {
    final b = AuthCard.fromJson({
      'kind': 'sign_in',
      'title': 'Sign in to gmail',
      'url': 'https://x.test/auth',
      'device_available': true,
      'notes': [],
    });
    expect(b.url, 'https://x.test/auth');
    expect(b.device, isFalse);
    expect(b.deviceAvailable, isTrue);
    final d = AuthCard.fromJson({
      'kind': 'sign_in',
      'title': 't',
      'user_code': 'ABCD-1234',
      'verification_uri': 'https://x.test/device',
      'device_available': false,
    });
    expect(d.device, isTrue);
    expect(d.userCode, 'ABCD-1234');
  });

  test('decodes close and change events', () {
    expect(
      TernEvent.fromJson({'auth_prompt_closed': 'a1'}),
      isA<AuthPromptClosed>().having((e) => e.id, 'id', 'a1'),
    );
    expect(
      TernEvent.fromJson({'connections_changed': {}}),
      isA<ConnectionsChanged>(),
    );
  });

  test('decodes the listing', () {
    final l = ConnectionList.fromJson({
      'connections': [jsonDecode(_conn)],
      'targets': [
        {
          'kind': 'connector',
          'name': 'gmail',
          'methods': ['oauth', 'key'],
          'key_names': ['GMAIL_KEY'],
        },
      ],
    });
    final a = l.accounts.single;
    expect(a.method, 'oauth');
    expect(a.isDefault, isTrue);
    expect(a.needsReauth, isTrue);
    expect(a.lastUsedAt, isNull);
    expect(l.targets.single.oauth && l.targets.single.key, isTrue);
  });

  group('over HTTP', () {
    late HttpServer server;
    late TernClient client;
    final seen = <String>[];

    setUp(() async {
      seen.clear();
      server = await HttpServer.bind('127.0.0.1', 0);
      server.listen((req) async {
        final body = await utf8.decoder.bind(req).join();
        seen.add('${req.method} ${req.uri.path} $body');
        req.response.headers.contentType = ContentType.json;
        if (req.uri.path == '/v1/connections' && req.method == 'POST') {
          req.response.write('{"status":"connected","connection":$_conn}');
        } else if (req.uri.path.endsWith('/reconnect')) {
          req.response.write('{"status":"reconnected","connection":$_conn}');
        } else if (req.method == 'DELETE') {
          req.response.write('{"ok":true}');
        } else {
          req.response.write(_conn);
        }
        await req.response.close();
      });
      client = TernClient(
        Endpoint.remote(
          Uri.parse('http://127.0.0.1:${server.port}'),
          token: 't',
        ),
      );
    });
    tearDown(() async {
      client.close();
      await server.close(force: true);
    });

    test('sends each action to its route', () async {
      final added = await client.addConnection(
        kind: 'connector',
        target: 'gmail',
        label: 'Work',
        method: 'key',
        values: {'GMAIL_KEY': 'k'},
      );
      expect(added.status, 'connected');
      final a = added.account;
      await client.reconnectConnection(a, device: true);
      await client.makeDefaultConnection(a);
      await client.renameConnection(a, 'New');
      await client.removeConnection(a);
      await client.answerAuthPrompt('a1', 'device');
      expect(seen, [
        'POST /v1/connections {"kind":"connector","target":"gmail",'
            '"label":"Work","method":"key","values":{"GMAIL_KEY":"k"}}',
        'POST /v1/connections/connector/gmail/work/reconnect '
            '{"device":true}',
        'POST /v1/connections/connector/gmail/work/default ',
        'PATCH /v1/connections/connector/gmail/work {"label":"New"}',
        'DELETE /v1/connections/connector/gmail/work ',
        'POST /v1/auth-prompts/a1 {"answer":"device"}',
      ]);
    });
  });
}
