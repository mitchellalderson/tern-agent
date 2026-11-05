// A long session loads in pages: the client sends limit/before, and reads
// total/offset (or copes with a daemon that sends neither).
import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

Map<String, Object?> _msg(int i) => {
  'role': 'user',
  'run_id': null,
  'content': [
    {'type': 'text', 'text': 'm$i'},
  ],
  'summary': false,
};

void main() {
  late Directory tmp;
  late HttpServer server;
  late TernClient client;
  final seen = <String>[];
  late Map<String, Object?> Function(Uri) reply;

  setUp(() async {
    tmp = Directory.systemTemp.createTempSync('tern-pages');
    final path = '${tmp.path}/s.sock';
    server = HttpServer.listenOn(
      await ServerSocket.bind(
        InternetAddress(path, type: InternetAddressType.unix),
        0,
      ),
    );
    server.listen((req) async {
      seen.add(req.uri.toString());
      req.response
        ..headers.contentType = ContentType.json
        ..write(jsonEncode(reply(req.uri)));
      await req.response.close();
    });
    client = TernClient(Endpoint.unix(path));
    seen.clear();
  });

  tearDown(() async {
    await server.close(force: true);
    tmp.deleteSync(recursive: true);
  });

  test(
    'limit and before go on the request; total and offset come back',
    () async {
      reply = (uri) => {
        'id': 's1',
        'messages': [for (var i = 17; i < 20; i++) _msg(i)],
        'total': 20,
        'offset': 17,
      };
      final page = await client.session('s1', limit: 3);
      expect(seen.single, '/v1/sessions/s1?limit=3');
      expect((page.total, page.offset, page.hasEarlier), (20, 17, true));
      expect(page.messages, hasLength(3));

      await client.session('s1', limit: 3, before: 17);
      expect(seen.last, '/v1/sessions/s1?limit=3&before=17');
    },
  );

  test('a daemon without paging sends the whole session', () async {
    reply = (_) => {
      'id': 's1',
      'messages': [for (var i = 0; i < 5; i++) _msg(i)],
    };
    final s = await client.session('s1');
    expect(seen.single, '/v1/sessions/s1');
    expect((s.total, s.offset, s.hasEarlier), (5, 0, false));
  });
}
