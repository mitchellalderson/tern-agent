// What the AP3 client methods send, against a local HTTPS server with the
// self-signed certificate in test/fixtures (as remote_test.dart).
import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

const _fixtures = 'test/fixtures';

List<int> _der(String pem) {
  final body = pem
      .split('\n')
      .skipWhile((l) => !l.startsWith('-----BEGIN CERTIFICATE'))
      .skip(1)
      .takeWhile((l) => !l.startsWith('-----END'))
      .join();
  return base64.decode(body);
}

void main() {
  late HttpServer server;
  late TernClient client;
  final seen = <({String method, String path, Object? body})>[];
  var reply = <String, Object?>{};
  var status = 200;

  setUp(() async {
    seen.clear();
    reply = {};
    status = 200;
    final ctx = SecurityContext()
      ..useCertificateChain('$_fixtures/cert.pem')
      ..usePrivateKey('$_fixtures/key.pem');
    server = await HttpServer.bindSecure('127.0.0.1', 0, ctx);
    server.listen((req) async {
      final text = await utf8.decoder.bind(req).join();
      seen.add((
        method: req.method,
        path: req.uri.path,
        body: text.isEmpty ? null : jsonDecode(text),
      ));
      req.response
        ..statusCode = status
        ..headers.contentType = ContentType.json
        ..write(jsonEncode(reply));
      await req.response.close();
    });
    final pin = fingerprint(
      _der(File('$_fixtures/cert.pem').readAsStringSync()),
    );
    client = TernClient(
      Endpoint.remote(
        Uri.parse('https://127.0.0.1:${server.port}'),
        token: 't',
        pin: pin,
      ),
    );
  });
  tearDown(() async {
    client.close();
    await server.close(force: true);
  });

  const agent = {
    'id': 'tern:ops',
    'slug': 'ops',
    'name': 'Ops',
    'memory': 'private',
    'autonomy': 'supervised',
    'telegram': {'bot_token_secret': 'OPS_BOT'},
    'updated_at': 0,
  };

  test('setting a ceiling sends the level; none sends null', () async {
    reply = {...agent};
    final a = await client.setAgentAutonomy('tern:ops', 'supervised');
    expect(a.autonomy, 'supervised');
    expect(seen.last.method, 'PATCH');
    expect(seen.last.path, '/v1/agents/tern%3Aops');
    expect(seen.last.body, {'autonomy': 'supervised'});
    await client.setAgentAutonomy('tern:ops', null);
    // An explicit null: the API treats an absent key as "unchanged".
    expect(seen.last.body, {'autonomy': null});
  });

  test('a bot is set by the secret NAME and removed with null', () async {
    reply = {...agent};
    final a = await client.setAgentTelegram('tern:ops', 'OPS_BOT');
    expect(a.telegramSecret, 'OPS_BOT');
    expect(seen.last.body, {
      'telegram': {'bot_token_secret': 'OPS_BOT'},
    });
    await client.setAgentTelegram('tern:ops', null);
    expect(seen.last.body, {'telegram': null});
  });

  test('the daemon refusing a shared token reaches the caller', () async {
    status = 400;
    reply = {
      'error':
          'MAIN_BOT is the main bot\'s token; an agent\'s bot needs its own',
    };
    await expectLater(
      client.setAgentTelegram('tern:ops', 'MAIN_BOT'),
      throwsA(
        isA<TernException>().having(
          (e) => e.message,
          'message',
          contains('main bot'),
        ),
      ),
    );
  });

  test('creating an agent may carry a ceiling', () async {
    reply = {...agent};
    await client.createAgent(name: 'Ops', autonomy: 'readonly');
    expect(seen.last.method, 'POST');
    expect((seen.last.body as Map)['autonomy'], 'readonly');
    await client.createAgent(name: 'Ops');
    expect((seen.last.body as Map).containsKey('autonomy'), isFalse);
  });

  test('"Let agents decide" is its own field on the channel PATCH', () async {
    reply = {
      'id': 'c1',
      'slug': 'launch',
      'title': 'Launch',
      'kind': 'topic',
      'updated_at': 0,
      'agents': {'members': [], 'let_agents_decide': true},
    };
    final c = await client.updateChannel('c1', letAgentsDecide: true);
    expect(seen.last.body, {'let_agents_decide': true});
    expect(c.agents!.letAgentsDecide, isTrue);
    await client.updateChannel('c1', letAgentsDecide: false);
    expect(seen.last.body, {'let_agents_decide': false});
    await client.updateChannel('c1', title: 'x');
    expect((seen.last.body as Map).containsKey('let_agents_decide'), isFalse);
  });
}
