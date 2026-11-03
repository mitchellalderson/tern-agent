// Provider setup against a real daemon and a fake OpenAI-compatible
// server. Needs the tern binary:
//   TERN_BIN=../../../../target/debug/tern dart test test/providers_live_test.dart
@Tags(['live'])
library;

import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

/// Answers `GET /v1/models` for the key `sk-good`, 401 otherwise.
Future<HttpServer> fakeProvider() async {
  final server = await HttpServer.bind('127.0.0.1', 0);
  server.listen((req) async {
    final ok = req.headers.value('authorization') == 'Bearer sk-good';
    if (req.uri.path != '/v1/models') {
      req.response.statusCode = 404;
    } else if (!ok) {
      req.response.statusCode = 401;
    } else {
      req.response.headers.contentType = ContentType.json;
      req.response.write(
        jsonEncode({
          'data': [
            {'id': 'gpt-a', 'created': 2},
            {'id': 'gpt-b', 'created': 1},
          ],
        }),
      );
    }
    await req.response.close();
  });
  return server;
}

void main() {
  final bin = Platform.environment['TERN_BIN'];

  test('verify, save, list, set default and remove', () async {
    final fake = await fakeProvider();
    final tmp = await Directory.systemTemp.createTemp('tern-providers');
    final home = '${tmp.path}/.tern';
    await Directory(home).create();
    final daemon = await Process.start(
      File(bin!).absolute.path,
      ['daemon'],
      environment: {
        'PATH': Platform.environment['PATH'] ?? '',
        'HOME': tmp.path,
        'TERN_HOME': home,
      },
      includeParentEnvironment: false,
    );
    final client = TernClient(Endpoint.local({'TERN_HOME': home}));
    addTearDown(() async {
      client.close();
      daemon.kill();
      await daemon.exitCode;
      await fake.close(force: true);
      await tmp.delete(recursive: true);
    });
    final end = DateTime.now().add(const Duration(seconds: 10));
    while (true) {
      try {
        await client.health();
        break;
      } on TernException {
        if (DateTime.now().isAfter(end)) rethrow;
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
    }

    expect((await client.info()).setupNeeded, isTrue);
    final base = 'http://127.0.0.1:${fake.port}';
    final bad = await client.verifyProvider(
      ProviderDraft(kind: ProviderKind.openaiCompat, baseUrl: base, key: 'x'),
    );
    expect(bad.ok, isFalse);
    expect(bad.error?.kind, 'unauthorized');

    final draft = ProviderDraft(
      kind: ProviderKind.openaiCompat,
      name: 'fake',
      baseUrl: base,
      key: 'sk-good',
    );
    final good = await client.verifyProvider(draft);
    expect(good.ok, isTrue);
    expect(good.suggestedBaseUrl, '$base/v1');
    expect(good.models.map((m) => m.id), ['gpt-a', 'gpt-b']);

    final saved = await client.saveProvider(draft, defaultModel: 'gpt-b');
    expect(saved.isDefault, isTrue);
    expect(saved.keySource, 'file');
    final info = await client.info();
    expect(info.defaultModel, 'fake:gpt-b');
    expect(info.setupNeeded, isFalse);

    expect((await client.providers()).single.name, 'fake');
    final lists = await client.models();
    expect(lists.single.models.length, 2);
    expect((await client.checkModel('gpt-a')).model, 'fake:gpt-a');
    expect((await client.setDefaultModel('gpt-a')).model, 'fake:gpt-a');

    await client.removeProvider('fake');
    expect((await client.info()).setupNeeded, isTrue);
  }, skip: bin == null ? 'set TERN_BIN to run' : null);
}
