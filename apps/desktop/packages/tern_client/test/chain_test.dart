import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

ProviderModels _p(String name, List<String> ids, {String? label}) =>
    ProviderModels(
      provider: name,
      label: label,
      models: [for (final i in ids) ModelInfo(id: i)],
    );

void main() {
  group('parseModelRef', () {
    test('splits at the first colon only', () {
      expect(parseModelRef('anthropic:claude-sonnet-4-5'), (
        provider: 'anthropic',
        id: 'claude-sonnet-4-5',
      ));
      expect(parseModelRef('ollama:llama3:8b'), (
        provider: 'ollama',
        id: 'llama3:8b',
      ));
      expect(parseModelRef(' a:b '), (provider: 'a', id: 'b'));
    });

    test('rejects what is not provider:model', () {
      for (final s in ['', 'nocolon', ':x', 'x:', '  ']) {
        expect(parseModelRef(s), isNull, reason: s);
      }
    });
  });

  test('a chain goes to the API as null, one model, or a list', () {
    expect(chainToWire([]), isNull);
    expect(chainToWire(['a:1']), 'a:1');
    expect(chainToWire(['a:1', 'b:2']), ['a:1', 'b:2']);
  });

  group('checkModelChain', () {
    final lists = [
      _p('anthropic', ['claude-sonnet-4-5', 'claude-haiku-4-5']),
      _p('openrouter', ['vendor/model'], label: 'OpenRouter'),
      const ProviderModels(provider: 'local', models: []),
    ];

    test('a good chain has no issues', () {
      expect(
        checkModelChain([
          'anthropic:claude-sonnet-4-5',
          'openrouter:vendor/model',
        ], lists: lists),
        isEmpty,
      );
    });

    test('a provider that is not connected blocks, in plain words', () {
      final i = checkModelChain(['gemini:flash'], lists: lists).single;
      expect(i.kind, ChainIssueKind.unknownProvider);
      expect(i.blocking, isTrue);
      expect(i.message, contains('"gemini"'));
      expect(i.message, contains('Connect it in Models'));
    });

    test('a model the list lacks only warns', () {
      final i = checkModelChain([
        'anthropic:claude-opus-9',
      ], lists: lists).single;
      expect(i.kind, ChainIssueKind.notListed);
      expect(i.blocking, isFalse);
      expect(i.message, contains('anthropic'));
    });

    test(
      'a provider whose list could not be fetched is not second-guessed',
      () {
        expect(checkModelChain(['local:anything'], lists: lists), isEmpty);
      },
    );

    test('a bad format and a repeat are caught without any lists', () {
      final issues = checkModelChain(['nocolon', 'a:1', 'a:1']);
      expect(issues.map((i) => (i.index, i.kind)), [
        (0, ChainIssueKind.badFormat),
        (2, ChainIssueKind.duplicate),
      ]);
      expect(issues.every((i) => i.blocking), isTrue);
    });
  });

  group('chainSaveMessage', () {
    test('says what to do for each kind of failure', () {
      expect(
        chainSaveMessage(const TernException('forbidden', status: 403)),
        contains("isn't allowed to change models"),
      );
      expect(
        chainSaveMessage(const TernException("can't reach tern")),
        contains('Nothing was changed'),
      );
      expect(
        chainSaveMessage(
          const TernException('expected provider:model', status: 400),
        ),
        contains('anthropic:claude-sonnet-4-5'),
      );
      expect(
        chainSaveMessage(
          const TernException(
            'a model list needs at least one entry',
            status: 400,
          ),
        ),
        'A list needs at least one model.',
      );
      // The daemon's own sentence is kept when it has one.
      expect(
        chainSaveMessage(const TernException('no provider "x"', status: 400)),
        'no provider "x"',
      );
    });
  });

  group('default_model chain over the config API', () {
    late HttpServer server;
    final bodies = <Map<String, dynamic>>[];
    final paths = <String>[];
    var status = 200;
    var reply = <String, dynamic>{};

    setUp(() async {
      bodies.clear();
      paths.clear();
      status = 200;
      reply = {
        'key': 'default_model',
        'kind': 'model',
        'value': ['a:1', 'b:2'],
      };
      server = await HttpServer.bind('127.0.0.1', 0);
      server.listen((req) async {
        final text = await utf8.decoder.bind(req).join();
        paths.add('${req.method} ${req.uri.path}');
        if (text.isNotEmpty)
          bodies.add(jsonDecode(text) as Map<String, dynamic>);
        req.response
          ..statusCode = status
          ..headers.contentType = ContentType.json
          ..write(jsonEncode(reply));
        await req.response.close();
      });
    });
    tearDown(() => server.close(force: true));

    TernClient client() => TernClient(
      Endpoint.remote(Uri.parse('http://127.0.0.1:${server.port}'), token: 't'),
    );

    test('writes the same setting the config editor does', () async {
      final c = client();
      addTearDown(c.close);
      final s = await c.setDefaultModelChain(['a:1', 'b:2']);
      expect(paths.single, 'PUT /v1/config/settings/default_model');
      expect(bodies.single, {
        'value': ['a:1', 'b:2'],
      });
      expect(s.value, ['a:1', 'b:2']);
    });

    test('one model is written as a plain string, as before', () async {
      final c = client();
      addTearDown(c.close);
      await c.setDefaultModelChain(['a:1']);
      expect(bodies.single, {'value': 'a:1'});
    });

    test('an empty chain is refused before anything is sent', () {
      final c = client();
      addTearDown(c.close);
      expect(() => c.setDefaultModelChain([]), throwsArgumentError);
      expect(paths, isEmpty);
    });

    test("the daemon's complaint comes back as the exception", () async {
      status = 400;
      reply = {'error': 'expected provider:model'};
      final c = client();
      addTearDown(c.close);
      await expectLater(
        c.setDefaultModelChain(['nocolon']),
        throwsA(
          isA<TernException>()
              .having((e) => e.status, 'status', 400)
              .having((e) => e.message, 'message', 'expected provider:model'),
        ),
      );
    });
  });
}
