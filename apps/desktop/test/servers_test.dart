// Several saved servers: the list in `desktop.json` and its migration, the
// Keychain tokens (one per server, never crossing), switching, the
// switcher menu, the manage dialog and the add/edit dialog.
import 'dart:convert';
import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/connection.dart';
import 'package:tern_desktop/state/daemon.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/servers.dart';

class MemTokens implements TokenStore {
  final tokens = <String, String>{};

  @override
  Future<String?> read(String account) async => tokens[account];

  @override
  Future<void> write(String account, String token) async =>
      tokens[account] = token;

  @override
  Future<void> delete(String account) async => tokens.remove(account);
}

/// Codes `CODE-<host>` pair with the token `tok-<host>`; anything else is a
/// wrong code. A host listed in [down] is unreachable.
class ServerWorld {
  final down = <String>{};
  final paired = <String>[];

  Future<Pairing> pair(PairingLink link) async {
    if (down.contains(link.url.host)) {
      throw TernException(
        "can't reach tern at ${link.url}: Connection refused",
      );
    }
    if (link.code != 'CODE-${link.url.host.toUpperCase()}') {
      throw const TernException('invalid or expired code', status: 401);
    }
    paired.add(link.url.host);
    return Pairing(
      deviceId: 'dev-${link.url.host}',
      scope: 'owner',
      endpoint: RemoteEndpoint(
        link.url,
        token: 'tok-${link.url.host}',
        pin: link.fingerprint,
      ),
    );
  }

  /// `/healthz` with no token.
  final probed = <Uri>[];
  Future<void> probe(Uri url, {String? pin}) async {
    probed.add(url);
    if (down.contains(url.host)) {
      throw TernException("can't reach tern at $url: Connection refused");
    }
    if (url.host == 'not-tern.example.net') {
      throw const TernException('Not Found', status: 404);
    }
  }

  Future<ServerStatus> check(Profile p) async => switch (p) {
    LocalProfile() => const ServerStatus(Reach.online, null, '0.2.0'),
    RemoteProfile r when down.contains(r.url.host) => const ServerStatus(
      Reach.offline,
      "Couldn't reach the server.",
    ),
    RemoteProfile() => const ServerStatus(Reach.online, null, '0.2.0'),
  };
}

class Env {
  Env({String? json}) {
    dir = Directory.systemTemp.createTempSync('tern_servers');
    file = File('${dir.path}/desktop.json');
    if (json != null) file.writeAsStringSync(json);
    reload();
  }

  late final Directory dir;
  late final File file;
  late AppPrefs prefs;
  final tokens = MemTokens();
  final world = ServerWorld();
  final applied = <Endpoint>[];
  late Connections connections;

  void reload() {
    prefs = AppPrefs.fromFile(file);
    connections = Connections(
      prefs: prefs,
      tokens: tokens,
      apply: applied.add,
      pair: world.pair,
      probeUrl: world.probe,
      check: world.check,
    );
  }

  void dispose() => dir.deleteSync(recursive: true);
}

PairingLink linkFor(String host, {String? name}) => PairingLink(
  url: Uri.parse('https://$host:7420'),
  code: 'CODE-${host.toUpperCase()}',
  name: name,
);

Widget app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: child),
);

/// Lets real file IO (saving desktop.json) finish between frames.
Future<void> settle(WidgetTester tester) async {
  for (var i = 0; i < 8; i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 50)),
    );
    await tester.pumpAndSettle();
  }
}

void main() {
  group('migrating the single saved server', () {
    const legacy =
        '{"on_close":"keep_running","connection":{"kind":"remote",'
        '"url":"https://home.example.net:7420","name":"Home server",'
        '"pin":"sha256:ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB",'
        '"device_id":"d1","scope":"owner"},"read":{"home":42}}';

    test(
      'becomes the first of the list, token and unread marks kept',
      () async {
        final env = Env(json: legacy);
        addTearDown(env.dispose);
        // The token is where the old app put it: under the URL.
        env.tokens.tokens['https://home.example.net:7420'] = 'tern_dev_old';

        final servers = env.connections.servers;
        expect(servers, hasLength(1));
        expect(servers.single.name, 'Home server');
        expect(env.connections.activeId, servers.single.id);
        expect(env.prefs.connection, same(servers.single));
        expect(env.prefs.read, {'home': 42});

        final e = await endpointFor(env.prefs.connection, env.tokens);
        expect(e, isA<RemoteEndpoint>());
        e as RemoteEndpoint;
        expect(e.token, 'tern_dev_old');
        expect(e.base, Uri.parse('https://home.example.net:7420'));
        expect(e.pin, startsWith('sha256:'));

        // Saved again, it reads back the same, with no token in the file.
        await env.prefs.save();
        final text = env.file.readAsStringSync();
        expect(text, isNot(contains('tern_dev_old')));
        final json = jsonDecode(text) as Map<String, dynamic>;
        expect((json['servers'] as List), hasLength(1));
        env.reload();
        expect(env.connections.servers.single.id, servers.single.id);
        expect(env.prefs.connection!.id, servers.single.id);
        expect(env.prefs.read, {'home': 42});
      },
    );

    test('a local choice keeps its unread marks, and the list is empty', () {
      final env = Env(json: '{"connection":{"kind":"local"},"read":{"a":1}}');
      addTearDown(env.dispose);
      expect(env.connections.servers, isEmpty);
      expect(env.connections.activeId, LocalProfile.localId);
      expect(env.prefs.read, {'a': 1});
    });

    test('unreadable or missing files mean no choice yet', () {
      final env = Env(json: 'not json');
      addTearDown(env.dispose);
      expect(env.prefs.connection, isNull);
      expect(env.connections.servers, isEmpty);
      expect(env.connections.activeId, LocalProfile.localId);
    });
  });

  group('unread marks are kept per server', () {
    test('the same channel id on two servers never shares a mark', () async {
      final env = Env();
      addTearDown(env.dispose);
      await env.connections.connect(linkFor('a.example.net'));
      env.prefs.read['home'] = 10;
      await env.connections.connect(linkFor('b.example.net'));
      expect(env.prefs.read, isEmpty);
      env.prefs.read['home'] = 99;
      await env.prefs.save();

      env.reload();
      final a = env.connections.servers.first.id;
      final b = env.connections.servers.last.id;
      expect(env.connections.activeId, b);
      expect(env.prefs.read, {'home': 99});
      await env.connections.switchTo(a);
      expect(env.prefs.read, {'home': 10});
    });
  });

  group('Connections', () {
    test(
      'adding keeps every server, each token under its own account',
      () async {
        final env = Env();
        addTearDown(env.dispose);
        final a = await env.connections.connect(
          linkFor('a.example.net', name: 'Home'),
        );
        final b = await env.connections.connect(
          linkFor('b.example.net', name: 'Work'),
        );
        expect(env.connections.servers.map((s) => s.name), ['Home', 'Work']);
        expect(a.account, isNot(b.account));
        expect(env.tokens.tokens, {
          a.account: 'tok-a.example.net',
          b.account: 'tok-b.example.net',
        });
        // The newest is in use.
        expect(env.connections.activeId, b.id);
        expect(env.file.readAsStringSync(), isNot(contains('tok-')));
      },
    );

    test('switching never sends one server\'s token to another', () async {
      final env = Env();
      addTearDown(env.dispose);
      final a = await env.connections.connect(linkFor('a.example.net'));
      final b = await env.connections.connect(linkFor('b.example.net'));
      env.applied.clear();

      await env.connections.switchTo(a.id);
      await env.connections.switchTo(b.id);
      await env.connections.useLocal();
      await env.connections.switchTo(a.id);

      expect(
        [
          for (final e in env.applied)
            switch (e) {
              RemoteEndpoint(:final base, :final token) =>
                '${base.host} $token',
              UnixEndpoint() => 'local',
            },
        ],
        [
          'a.example.net tok-a.example.net',
          'b.example.net tok-b.example.net',
          'local',
          'a.example.net tok-a.example.net',
        ],
      );
      // What is saved says the same.
      expect(AppPrefs.fromFile(env.file).connection!.id, a.id);
    });

    test('switching to the one in use does nothing', () async {
      final env = Env();
      addTearDown(env.dispose);
      final a = await env.connections.connect(linkFor('a.example.net'));
      env.applied.clear();
      await env.connections.switchTo(a.id);
      expect(env.applied, isEmpty);
    });

    test(
      'a server without its token, or unknown, is refused in words',
      () async {
        final env = Env();
        addTearDown(env.dispose);
        final a = await env.connections.connect(linkFor('a.example.net'));
        await env.connections.useLocal();
        env.tokens.tokens.clear();
        env.applied.clear();
        await expectLater(
          env.connections.switchTo(a.id),
          throwsA(
            isA<FormatException>().having(
              (e) => e.message,
              'message',
              contains('signed out of a.example.net'),
            ),
          ),
        );
        await expectLater(
          env.connections.switchTo('nope'),
          throwsA(isA<FormatException>()),
        );
        // Nothing changed: still on this computer.
        expect(env.applied, isEmpty);
        expect(env.connections.activeId, LocalProfile.localId);
        expect(AppPrefs.fromFile(env.file).connection, isA<LocalProfile>());
      },
    );

    test('pairing the same address again updates it in place', () async {
      final env = Env();
      addTearDown(env.dispose);
      final a = await env.connections.connect(
        linkFor('a.example.net', name: 'Home'),
      );
      env.tokens.tokens[a.account] = 'stale';
      final again = await env.connections.connect(linkFor('a.example.net'));
      expect(again.id, a.id);
      expect(again.name, 'Home');
      expect(env.connections.servers, hasLength(1));
      expect(env.tokens.tokens[a.account], 'tok-a.example.net');
    });

    test(
      'names are checked: not empty, unique, not the built-in one',
      () async {
        final env = Env();
        addTearDown(env.dispose);
        await env.connections.connect(linkFor('a.example.net', name: 'Home'));
        expect(env.connections.nameProblem(''), contains('Give it a name'));
        expect(env.connections.nameProblem('x' * 41), contains('40'));
        expect(env.connections.nameProblem('home'), contains('already have'));
        expect(
          env.connections.nameProblem(localName),
          contains('already have'),
        );
        expect(env.connections.nameProblem('Work'), isNull);
        // A second server with the same suggested name gets a free one.
        final dup = await env.connections.connect(
          linkFor('b.example.net', name: 'Home'),
        );
        expect(dup.name, 'Home 2');
        await expectLater(
          env.connections.connect(linkFor('c.example.net'), name: 'HOME'),
          throwsA(isA<FormatException>()),
        );
        expect(env.world.paired, ['a.example.net', 'b.example.net']);
      },
    );

    test('rename keeps the connection and the token', () async {
      final env = Env();
      addTearDown(env.dispose);
      final a = await env.connections.connect(linkFor('a.example.net'));
      env.applied.clear();
      await env.connections.rename(a.id, 'Home server');
      expect(env.connections.nameOf(a.id), 'Home server');
      expect(env.prefs.connection, same(env.connections.server(a.id)));
      expect(env.tokens.tokens[a.account], 'tok-a.example.net');
      expect(env.applied, isEmpty);
      expect(AppPrefs.fromFile(env.file).servers.single.name, 'Home server');
    });

    test(
      'pairing again at a new address forgets the old key only after',
      () async {
        final env = Env();
        addTearDown(env.dispose);
        final a = await env.connections.connect(linkFor('a.example.net'));
        env.applied.clear();

        // A wrong code: nothing changes, the old key stays.
        await expectLater(
          env.connections.repair(
            a.id,
            PairingLink(
              url: Uri.parse('https://new.example.net'),
              code: 'WRONG',
            ),
          ),
          throwsA(isA<TernException>()),
        );
        expect(env.connections.server(a.id)!.url.host, 'a.example.net');
        expect(env.tokens.tokens[a.account], 'tok-a.example.net');
        expect(env.applied, isEmpty);

        final moved = await env.connections.repair(
          a.id,
          PairingLink(
            url: Uri.parse('https://new.example.net'),
            code: 'CODE-NEW.EXAMPLE.NET',
          ),
        );
        expect(moved.id, a.id);
        expect(moved.url.host, 'new.example.net');
        // The old key is replaced, never carried to the new address.
        expect(env.tokens.tokens, {a.account: 'tok-new.example.net'});
        // It was in use, so the app now talks to the new address.
        final e = env.applied.single as RemoteEndpoint;
        expect(
          (e.base.host, e.token),
          ('new.example.net', 'tok-new.example.net'),
        );
      },
    );

    test('an address already saved under another name is refused', () async {
      final env = Env();
      addTearDown(env.dispose);
      await env.connections.connect(linkFor('a.example.net', name: 'Home'));
      final b = await env.connections.connect(linkFor('b.example.net'));
      await expectLater(
        env.connections.repair(b.id, linkFor('a.example.net')),
        throwsA(
          isA<FormatException>().having(
            (e) => e.message,
            'message',
            contains('"Home"'),
          ),
        ),
      );
    });

    test('removing the one in use goes back to this computer', () async {
      final env = Env();
      addTearDown(env.dispose);
      final a = await env.connections.connect(linkFor('a.example.net'));
      final b = await env.connections.connect(linkFor('b.example.net'));
      env.applied.clear();

      await env.connections.remove(b.id);
      expect(env.applied.single, isA<UnixEndpoint>());
      expect(env.connections.activeId, LocalProfile.localId);
      expect(env.tokens.tokens, {a.account: 'tok-a.example.net'});
      expect(env.connections.servers.map((s) => s.id), [a.id]);

      // Removing one that is not in use leaves the connection alone.
      env.applied.clear();
      await env.connections.remove(a.id);
      expect(env.applied, isEmpty);
      expect(env.tokens.tokens, isEmpty);
      expect(AppPrefs.fromFile(env.file).servers, isEmpty);
    });

    test('this computer can\'t be removed', () async {
      final env = Env();
      addTearDown(env.dispose);
      await expectLater(
        env.connections.remove(LocalProfile.localId),
        throwsA(isA<FormatException>()),
      );
    });

    test('two quick switches end on the last one, saved and applied', () async {
      final env = Env();
      addTearDown(env.dispose);
      final a = await env.connections.connect(linkFor('a.example.net'));
      final b = await env.connections.connect(linkFor('b.example.net'));
      env.applied.clear();
      await Future.wait([
        env.connections.switchTo(a.id),
        env.connections.switchTo(b.id),
        env.connections.switchTo(a.id),
      ]);
      expect((env.applied.last as RemoteEndpoint).base.host, 'a.example.net');
      expect(env.connections.activeId, a.id);
      expect(AppPrefs.fromFile(env.file).connection!.id, a.id);
    });
  });

  group('addresses', () {
    test('plain http is refused for another computer, in every door', () async {
      expect(
        () => parseServerInput('http://home.example.net:7420', code: 'X'),
        throwsA(
          isA<FormatException>().having(
            (e) => e.message,
            'message',
            contains('https://'),
          ),
        ),
      );
      expect(
        () => parseServerInput(
          'tern://pair?u=http%3A%2F%2Fhome.example.net%3A7420&c=X',
        ),
        throwsFormatException,
      );
      // This computer may use http.
      expect(
        parseServerInput('http://localhost:7420', code: 'x').url.scheme,
        'http',
      );

      // A server saved as http by an older app is not used, not trusted.
      final env = Env(
        json:
            '{"connection":{"kind":"remote","url":"http://home.example.net:7420",'
            '"name":"Old"}}',
      );
      addTearDown(env.dispose);
      env.tokens.tokens['http://home.example.net:7420'] = 'tok';
      expect(await endpointFor(env.prefs.connection, env.tokens), isNull);
      final s = await checkServer(env.prefs.connection!, env.tokens);
      expect(s.reach, Reach.unusable);
      expect(s.message, contains('https://'));
      await expectLater(
        env.connections.switchTo(
          env.connections.servers.single.id,
          force: true,
        ),
        throwsA(isA<FormatException>()),
      );
    });

    test('a bare host gets https and loses a trailing slash', () {
      expect(
        parseServerUrl('home.example.net:7420/'),
        Uri.parse('https://home.example.net:7420'),
      );
      expect(
        parseServerUrl('https://home.example.net'),
        Uri.parse('https://home.example.net'),
      );
      expect(
        () => parseServerUrl('https://u:p@x.example'),
        throwsFormatException,
      );
      expect(() => parseServerUrl('ftp://x'), throwsFormatException);
    });

    test('errors are in plain words', () {
      expect(
        serverErrorText(const TernException('x', status: 401)),
        contains('expire after 10 minutes'),
      );
      expect(
        serverErrorText(const TernException('x', status: 404)),
        contains("doesn't look like a tern server"),
      );
      expect(
        serverErrorText(
          const TernException("can't establish a secure connection to x: bad"),
        ),
        contains("isn't the one this app expects"),
      );
      expect(
        serverErrorText(const TernException("can't reach tern at x: refused")),
        contains("Couldn't reach the server"),
      );
      expect(
        serverErrorText(const TernException('moved', status: 302)),
        contains('somewhere else'),
      );
      expect(
        serverErrorText(const SignedOutException('x', core: 'x')),
        contains('no longer accepts this device'),
      );
    });
  });

  group('the switcher', () {
    testWidgets('names the server in use and switches from the menu', (
      tester,
    ) async {
      final env = Env();
      addTearDown(env.dispose);
      await tester.runAsync(() async {
        await env.connections.connect(linkFor('a.example.net', name: 'Home'));
        await env.connections.connect(
          linkFor('b.example.net', name: 'Work box'),
        );
      });
      env.world.down.add('a.example.net');
      env.applied.clear();

      await tester.pumpWidget(
        app(
          Builder(
            builder: (context) => Align(
              alignment: Alignment.bottomLeft,
              child: SizedBox(
                width: 240,
                child: ServerSwitcher(
                  connections: env.connections,
                  statusColor: Colors.green,
                  statusText: 'Connected · tern 0.2.0',
                ),
              ),
            ),
          ),
        ),
      );
      expect(find.text('Work box'), findsOneWidget);
      expect(find.text('Connected · tern 0.2.0'), findsOneWidget);

      await tester.tap(find.byKey(const Key('server-switcher')));
      await tester.pumpAndSettle();
      // Every server, with how it is doing.
      expect(
        find.byKey(Key('server-row-${LocalProfile.localId}')),
        findsOneWidget,
      );
      expect(find.text('Home'), findsOneWidget);
      expect(find.text('Not reachable'), findsOneWidget);
      expect(find.text('Add a server…'), findsOneWidget);
      expect(find.text('Manage servers…'), findsOneWidget);

      await tester.tap(find.text('Home'));
      await settle(tester);
      expect(env.connections.nameOf(env.connections.activeId), 'Home');
      expect((env.applied.single as RemoteEndpoint).token, 'tok-a.example.net');
    });
  });

  group('the manage dialog', () {
    testWidgets('lists servers with status; this computer can\'t be removed', (
      tester,
    ) async {
      final env = Env();
      addTearDown(env.dispose);
      await tester.runAsync(() async {
        await env.connections.connect(linkFor('a.example.net', name: 'Home'));
        await env.connections.useLocal();
      });
      env.world.down.add('a.example.net');
      await tester.pumpWidget(app(ServersDialog(connections: env.connections)));
      await tester.pumpAndSettle();

      expect(find.text(localName), findsOneWidget);
      expect(find.text('In use'), findsOneWidget);
      expect(
        find.text('Built in · tern running on this computer'),
        findsOneWidget,
      );
      expect(find.text('Home'), findsOneWidget);
      expect(find.text("Couldn't reach the server."), findsOneWidget);
      // One remove button (Home's), none for the built-in entry.
      expect(find.byTooltip('Remove from this app'), findsOneWidget);
      expect(
        find.byTooltip('Rename, change address or pair again'),
        findsOneWidget,
      );
    });

    testWidgets('switching closes the dialog; removing asks first', (
      tester,
    ) async {
      final env = Env();
      addTearDown(env.dispose);
      await tester.runAsync(() async {
        await env.connections.connect(linkFor('a.example.net', name: 'Home'));
        await env.connections.useLocal();
      });
      env.applied.clear();
      late BuildContext ctx;
      await tester.pumpWidget(
        app(
          Builder(
            builder: (c) {
              ctx = c;
              return const SizedBox();
            },
          ),
        ),
      );
      unawaited(showServersDialog(ctx, env.connections));
      await tester.pumpAndSettle();

      // Cancel keeps it.
      await tester.tap(find.byTooltip('Remove from this app'));
      await tester.pumpAndSettle();
      expect(find.text('Remove Home?'), findsOneWidget);
      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();
      expect(env.connections.servers, hasLength(1));

      await tester.tap(find.byKey(const Key('switch-server')));
      await settle(tester);
      expect(find.byType(ServersDialog), findsNothing);
      expect(env.connections.nameOf(env.connections.activeId), 'Home');

      unawaited(showServersDialog(ctx, env.connections));
      await tester.pumpAndSettle();
      await tester.tap(find.byTooltip('Remove from this app'));
      await tester.pumpAndSettle();
      // It is the one in use: the dialog says it goes back to this computer.
      expect(find.textContaining('goes back to'), findsOneWidget);
      await tester.tap(find.byKey(const Key('confirm-remove-server')));
      await settle(tester);
      expect(env.connections.servers, isEmpty);
      expect(env.tokens.tokens, isEmpty);
      expect(env.connections.activeId, LocalProfile.localId);
      expect(env.applied.last, isA<UnixEndpoint>());
    });
  });

  group('adding and editing', () {
    testWidgets('test before saving: found, not tern, unreachable, http', (
      tester,
    ) async {
      final env = Env();
      addTearDown(env.dispose);
      await tester.pumpWidget(
        app(ServerEditDialog(connections: env.connections)),
      );
      await tester.pumpAndSettle();
      expect(find.text('Add a server'), findsOneWidget);
      // Nothing to test yet.
      expect(
        tester
            .widget<OutlinedButton>(find.byKey(const Key('test-connection')))
            .onPressed,
        isNull,
      );

      Future<String> testing(String address) async {
        await tester.enterText(find.byType(TextField).at(1), address);
        await tester.pumpAndSettle();
        await tester.tap(find.byKey(const Key('test-connection')));
        await settle(tester);
        return tester.widget<Text>(find.byKey(const Key('test-result'))).data!;
      }

      expect(
        await testing('a.example.net:7420'),
        contains('Found a tern server at a.example.net'),
      );
      expect(
        await testing('not-tern.example.net'),
        contains("doesn't look like a tern server"),
      );
      env.world.down.add('down.example.net');
      expect(
        await testing('down.example.net'),
        contains("Couldn't reach the server"),
      );
      expect(await testing('http://plain.example.net'), contains('https://'));
      // Testing sent no token and saved nothing.
      expect(env.world.probed.map((u) => u.host), [
        'a.example.net',
        'not-tern.example.net',
        'down.example.net',
      ]);
      expect(env.connections.servers, isEmpty);
      expect(env.tokens.tokens, isEmpty);
    });

    testWidgets('adding pairs, saves, and switches when asked', (tester) async {
      final env = Env();
      addTearDown(env.dispose);
      final results = <bool?>[];
      late BuildContext ctx;
      await tester.pumpWidget(
        app(
          Builder(
            builder: (c) {
              ctx = c;
              return const SizedBox();
            },
          ),
        ),
      );
      unawaited(
        showDialog<bool>(
          context: ctx,
          builder: (_) => ServerEditDialog(connections: env.connections),
        ).then(results.add),
      );
      await tester.pumpAndSettle();

      await tester.enterText(
        find.byKey(const Key('server-name-field')),
        'Work box',
      );
      await tester.enterText(find.byType(TextField).at(1), 'b.example.net');
      await tester.pumpAndSettle();
      // A wrong code says what to do and saves nothing.
      await tester.enterText(find.byType(TextField).at(2), 'nope');
      await tester.tap(find.byKey(const Key('save-server')));
      await settle(tester);
      expect(find.textContaining('expire after 10 minutes'), findsOneWidget);
      expect(env.connections.servers, isEmpty);

      await tester.enterText(
        find.byType(TextField).at(2),
        'code-b.example.net',
      );
      await tester.tap(find.byKey(const Key('save-server')));
      await settle(tester);
      expect(results, [true]);
      expect(env.connections.servers.single.name, 'Work box');
      expect(env.connections.activeId, env.connections.servers.single.id);
      expect(env.tokens.tokens.values, ['tok-b.example.net']);
    });

    testWidgets('editing: rename only; pairing again asks for a code', (
      tester,
    ) async {
      final env = Env();
      addTearDown(env.dispose);
      final a = await tester.runAsync(
        () => env.connections.connect(linkFor('a.example.net', name: 'Home')),
      );
      env.applied.clear();
      await tester.pumpWidget(
        app(ServerEditDialog(connections: env.connections, editing: a)),
      );
      await tester.pumpAndSettle();
      // Unchanged: nothing to save.
      expect(
        tester
            .widget<FilledButton>(find.byKey(const Key('save-server')))
            .onPressed,
        isNull,
      );
      expect(find.text('https://a.example.net:7420'), findsOneWidget);

      // Testing a saved server uses its own token.
      await tester.tap(find.byKey(const Key('test-connection')));
      await settle(tester);
      expect(
        tester.widget<Text>(find.byKey(const Key('test-result'))).data,
        'Connected to Home (tern 0.2.0).',
      );

      await tester.enterText(
        find.byKey(const Key('server-name-field')),
        'Work box',
      );
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('save-server')));
      await settle(tester);
      expect(env.connections.nameOf(a!.id), 'Work box');
      expect(env.applied, isEmpty);
    });

    testWidgets('a taken name is refused in words', (tester) async {
      final env = Env();
      addTearDown(env.dispose);
      await tester.runAsync(() async {
        await env.connections.connect(linkFor('a.example.net', name: 'Home'));
      });
      final b = await tester.runAsync(
        () => env.connections.connect(linkFor('b.example.net', name: 'Work')),
      );
      await tester.pumpWidget(
        app(ServerEditDialog(connections: env.connections, editing: b)),
      );
      await tester.pumpAndSettle();
      await tester.enterText(
        find.byKey(const Key('server-name-field')),
        'home',
      );
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('save-server')));
      await settle(tester);
      expect(
        find.text('You already have a server called "home".'),
        findsOneWidget,
      );
      expect(env.connections.nameOf(b!.id), 'Work');
    });
  });
}
