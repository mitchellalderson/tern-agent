// RemoteEndpoint against a local HTTPS server with a self-signed
// certificate (test/fixtures, made with openssl, valid for 100 years).
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

const _fixtures = 'test/fixtures';

/// DER bytes of the first certificate in a PEM file.
List<int> _der(String pem) {
  final body = pem
      .split('\n')
      .skipWhile((l) => !l.startsWith('-----BEGIN CERTIFICATE'))
      .skip(1)
      .takeWhile((l) => !l.startsWith('-----END'))
      .join();
  return base64.decode(body);
}

/// A fake core: answers what [handle] says and records every request.
class FakeCore {
  FakeCore._(this.server);

  final HttpServer server;
  final requests = <HttpRequest>[];
  final bodies = <List<int>>[];
  FutureOr<void> Function(HttpRequest req, List<int> body) handle =
      (req, body) => _json(req, 200, {'pid': 1, 'version': '0.1.0'});

  static Future<FakeCore> start() async {
    final ctx = SecurityContext()
      ..useCertificateChain('$_fixtures/cert.pem')
      ..usePrivateKey('$_fixtures/key.pem');
    final core = FakeCore._(await HttpServer.bindSecure('127.0.0.1', 0, ctx));
    core.server.listen((req) async {
      final body = await req.fold<List<int>>([], (a, b) => a..addAll(b));
      core.requests.add(req);
      core.bodies.add(body);
      await core.handle(req, body);
    });
    return core;
  }

  Uri get url => Uri.parse('https://127.0.0.1:${server.port}');

  Future<void> close() => server.close(force: true);
}

Future<void> _json(HttpRequest req, int status, Object body) async {
  req.response
    ..statusCode = status
    ..headers.contentType = ContentType.json
    ..write(jsonEncode(body));
  await req.response.close();
}

void main() {
  late FakeCore core;
  late String pin;

  setUp(() async {
    core = await FakeCore.start();
    pin = fingerprint(_der(File('$_fixtures/cert.pem').readAsStringSync()));
  });
  tearDown(() => core.close());

  group('pinning', () {
    test('accepts the pinned certificate and sends the token', () async {
      final client = TernClient(
        Endpoint.remote(core.url, token: 'tern_dev_abc', pin: pin),
      );
      addTearDown(client.close);
      final h = await client.health();
      expect(h.version, '0.1.0');
      expect(
        core.requests.single.headers.value('authorization'),
        'Bearer tern_dev_abc',
      );
    });

    test('pins compare case- and colon-insensitively', () async {
      final hex = pin.substring(7).toLowerCase();
      final colons = [
        for (var i = 0; i < hex.length; i += 2) hex.substring(i, i + 2),
      ].join(':');
      final client = TernClient(
        Endpoint.remote(core.url, token: 't', pin: 'sha256:$colons'),
      );
      addTearDown(client.close);
      await client.health();
    });

    test('rejects a different certificate before sending anything', () async {
      final client = TernClient(
        Endpoint.remote(core.url, token: 't', pin: 'sha256:${'00' * 32}'),
      );
      addTearDown(client.close);
      await expectLater(
        client.health(),
        throwsA(
          isA<TernException>().having(
            (e) => e.message,
            'message',
            contains("isn't the one this device paired with"),
          ),
        ),
      );
      expect(core.requests, isEmpty);
    });

    test('without a pin, a self-signed certificate is not trusted', () async {
      final client = TernClient(Endpoint.remote(core.url, token: 't'));
      addTearDown(client.close);
      await expectLater(client.health(), throwsA(isA<TernException>()));
      expect(core.requests, isEmpty);
    });

    test('malformed pins are refused', () {
      expect(() => parsePin('sha256:abcd'), throwsFormatException);
      expect(() => parsePin('sha256:${'zz' * 32}'), throwsFormatException);
      expect(parsePin(pin), hasLength(32));
    });
  });

  group('signed out', () {
    test('a 401 is a SignedOutException, with no retry', () async {
      core.handle = (req, _) =>
          _json(req, 401, {'error': 'this device was signed out'});
      final client = TernClient(
        Endpoint.remote(core.url, token: 'tern_dev_old', pin: pin),
      );
      addTearDown(client.close);
      await expectLater(
        client.sessions(),
        throwsA(
          isA<SignedOutException>()
              .having((e) => e.status, 'status', 401)
              .having((e) => e.message, 'message', startsWith('Signed out of')),
        ),
      );
      expect(core.requests, hasLength(1));
    });

    test('the event stream ends with SignedOutException', () async {
      core.handle = (req, _) => _json(req, 401, {'error': 'revoked'});
      final client = TernClient(
        Endpoint.remote(core.url, token: 'tern_dev_old', pin: pin),
      );
      addTearDown(client.close);
      final errors = <Object>[];
      await client.events().handleError(errors.add).toList();
      expect(errors.single, isA<SignedOutException>());
      expect(core.requests, hasLength(1));
    });

    test('forbidden is not signed out', () async {
      core.handle = (req, _) => _json(req, 403, {'error': 'needs owner'});
      final client = TernClient(
        Endpoint.remote(core.url, token: 'tern_dev_x', pin: pin),
      );
      addTearDown(client.close);
      await expectLater(
        client.devices(),
        throwsA(
          isA<TernException>()
              .having((e) => e is SignedOutException, 'signed out', isFalse)
              .having((e) => e.status, 'status', 403),
        ),
      );
    });
  });

  group('pairing', () {
    test('parses links', () {
      final link = PairingLink.parse(
        'tern://pair?u=https%3A%2F%2Fhome.example.ts.net%3A7420&c=K7QX9M2P'
        '&fp=$pin&n=Mitch%27s%20Mac',
      );
      expect(link.url, Uri.parse('https://home.example.ts.net:7420'));
      expect(link.code, 'K7QX9M2P');
      expect(link.fingerprint, pin);
      expect(link.name, "Mitch's Mac");
      expect(() => PairingLink.parse('https://x'), throwsFormatException);
      expect(() => PairingLink.parse('tern://pair?c=X'), throwsFormatException);
      expect(
        () => PairingLink.parse('tern://pair?u=https%3A%2F%2Fx&c=X&fp=nope'),
        throwsFormatException,
      );
    });

    test(
      'redeems a code without a token and returns a pinned endpoint',
      () async {
        core.handle = (req, body) {
          expect(req.uri.path, '/v1/auth/pair');
          expect(req.headers.value('authorization'), isNull);
          expect(jsonDecode(utf8.decode(body)), {
            'code': 'K7QX9M2P',
            'device_name': 'phone',
            'platform': 'ios',
          });
          return _json(req, 200, {
            'device_id': 'dev1',
            'token': 'tern_dev_new',
            'scope': 'chat',
          });
        };
        final link = PairingLink(
          url: core.url,
          code: 'K7QX9M2P',
          fingerprint: pin,
        );
        final p = await TernClient.pair(
          link,
          deviceName: 'phone',
          platform: 'ios',
        );
        expect(p.deviceId, 'dev1');
        expect(p.scope, 'chat');
        expect(p.endpoint.token, 'tern_dev_new');
        expect(p.endpoint.pin, pin);
      },
    );

    test('a bad code is a plain error, not signed out', () async {
      core.handle = (req, _) => _json(req, 401, {
        'error': 'the pairing code is invalid, used, or expired',
      });
      final link = PairingLink(url: core.url, code: 'NOPE', fingerprint: pin);
      await expectLater(
        TernClient.pair(link, deviceName: 'phone'),
        throwsA(
          isA<TernException>()
              .having((e) => e is SignedOutException, 'signed out', isFalse)
              .having((e) => e.message, 'message', contains('pairing code')),
        ),
      );
    });
  });

  test('health exposes capabilities, with local defaults', () {
    final remote = Health.fromJson({
      'pid': 1,
      'version': '0.1.0',
      'api': 1,
      'deployment': 'server',
      'auth': {'kind': 'device', 'scope': 'chat'},
      'features': ['devices', 'uploads'],
    });
    expect(remote.isLocal, isFalse);
    expect(remote.isOwner, isFalse);
    expect(remote.canChat, isTrue);
    expect(remote.features, contains('uploads'));
    final old = Health.fromJson({'pid': 1, 'version': '0.1.0'});
    expect(old.isLocal && old.isOwner, isTrue);
    expect(old.features, isEmpty);
  });

  test('uploads send one multipart file', () async {
    core.handle = (req, body) {
      expect(req.headers.contentType?.mimeType, 'multipart/form-data');
      final text = latin1.decode(body);
      expect(text, contains('filename="notes.txt"'));
      expect(text, contains('remember the milk'));
      return _json(req, 200, {'path': 'uploads/u1/notes.txt', 'size': 17});
    };
    final client = TernClient(Endpoint.remote(core.url, token: 't', pin: pin));
    addTearDown(client.close);
    final path = await client.upload(
      'notes.txt',
      utf8.encode('remember the milk'),
    );
    expect(path, 'uploads/u1/notes.txt');
  });
}
