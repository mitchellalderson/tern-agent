import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

const _event =
    '{"secret_requested":{"id":"q1","run_id":"r","session_id":"s",'
    '"call_id":"toolu_1","secrets":[{"name":"WEATHER_API_KEY",'
    '"in_file":false,"in_env":true,'
    '"used_by":["connector weather (draft) · reaches api.weather.example"]}],'
    '"reason":"to call the weather API","replace":true,'
    '"parent_run_id":"p","agent_label":"scout","root_session_id":"root"}}';

void main() {
  test('decodes secret requests', () {
    final e = TernEvent.fromJson(jsonDecode(_event) as Json);
    final r = (e as SecretRequested).request;
    expect(r.id, 'q1');
    expect(r.callId, 'toolu_1');
    expect(r.replace, isTrue);
    expect(r.agentLabel, 'scout');
    expect(r.rootSessionId, 'root');
    expect(r.secrets.single.name, 'WEATHER_API_KEY');
    expect(r.secrets.single.inEnv, isTrue);
    expect(r.secrets.single.inFile, isFalse);
    expect(r.secrets.single.usedBy, hasLength(1));
  });

  test('optional fields may be missing', () {
    final r = SecretRequest.fromJson({
      'id': 'q',
      'run_id': 'r',
      'session_id': 's',
      'secrets': [
        {'name': 'A', 'in_file': false, 'in_env': false, 'used_by': []},
      ],
      'reason': 'why',
      'replace': false,
    });
    expect(r.callId, isNull);
    expect(r.rootSessionId, isNull);
    expect(r.secrets.single.usedBy, isEmpty);
  });

  group('over HTTP', () {
    late HttpServer server;
    late TernClient client;
    final seen = <String>[];
    var answerStatus = 200;

    setUp(() async {
      seen.clear();
      answerStatus = 200;
      server = await HttpServer.bind('127.0.0.1', 0);
      server.listen((req) async {
        final body = await utf8.decoder.bind(req).join();
        seen.add('${req.method} ${req.uri.path} $body');
        req.response
          ..statusCode = req.method == 'POST' ? answerStatus : 200
          ..headers.contentType = ContentType.json;
        if (req.method == 'POST') {
          req.response.write(
            answerStatus == 200
                ? '{"ok":true}'
                : '{"error":"needs an owner device"}',
          );
        } else {
          req.response.write('[${_event.substring(20, _event.length - 1)}]');
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

    test('lists pending requests', () async {
      final rs = await client.secretRequests();
      expect(rs.single.id, 'q1');
      expect(seen.single, startsWith('GET /v1/secret-requests'));
    });

    test('posts the values, or an empty map to skip', () async {
      await client.answerSecretRequest('q1', {'WEATHER_API_KEY': 'abc'});
      await client.answerSecretRequest('q1', {});
      expect(
        seen[0],
        'POST /v1/secret-requests/q1 {"values":{"WEATHER_API_KEY":"abc"}}',
      );
      expect(seen[1], 'POST /v1/secret-requests/q1 {"values":{}}');
    });

    test('a 403 surfaces its status', () async {
      answerStatus = 403;
      await expectLater(
        client.answerSecretRequest('q1', {}),
        throwsA(isA<TernException>().having((e) => e.status, 'status', 403)),
      );
    });
  });
}
