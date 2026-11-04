import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/connection.dart';
import 'package:tern_desktop/state/daemon.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/settings.dart';
import 'package:tern_desktop/ui/where_to_run.dart';

class _MemTokens implements TokenStore {
  final tokens = <String, String>{};

  @override
  Future<String?> read(String account) async => tokens[account];

  @override
  Future<void> write(String account, String token) async =>
      tokens[account] = token;

  @override
  Future<void> delete(String account) async => tokens.remove(account);
}

/// A server that accepts code `GOODCODE` and nothing else.
Future<Pairing> _fakePair(PairingLink link) async {
  if (link.code != 'GOODCODE') {
    throw const TernException('invalid or expired code', status: 401);
  }
  return Pairing(
    deviceId: 'dev1',
    scope: 'owner',
    endpoint: RemoteEndpoint(
      link.url,
      token: 'tern_dev_secret',
      pin: link.fingerprint,
    ),
  );
}

class _Env {
  _Env({List<String> statusJson = const []}) {
    dir = Directory.systemTemp.createTempSync('tern_where');
    prefs = AppPrefs.fromFile(File('${dir.path}/desktop.json'));
    control = TernControl(
      bin: '/fake/tern',
      env: {'HOME': dir.path},
      run: (bin, args) async {
        runs.add(args);
        if (args.contains('--json') && statusJson.isNotEmpty) {
          return ProcessResult(0, 0, statusJson.first, '');
        }
        return ProcessResult(0, 0, '', '');
      },
    );
    connections = Connections(
      prefs: prefs,
      tokens: tokens,
      apply: applied.add,
      pair: _fakePair,
    );
  }

  late final Directory dir;
  late final AppPrefs prefs;
  late final TernControl control;
  late final Connections connections;
  final tokens = _MemTokens();
  final runs = <List<String>>[];
  final applied = <Endpoint>[];

  void dispose() => dir.deleteSync(recursive: true);
}

Widget _app(Widget Function(BuildContext) child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: Builder(builder: child)),
);

/// Lets real file IO (saving desktop.json) finish between frames.
Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 5; i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 30)),
    );
    await tester.pumpAndSettle();
  }
}

final _link =
    'tern://pair?u=https://home.example.net:7420&c=GOODCODE'
    '&fp=sha256:${'AB' * 32}&n=home-server';

void main() {
  group('parseServerInput', () {
    test('a pairing link carries everything', () {
      final l = parseServerInput(_link);
      expect(l.url, Uri.parse('https://home.example.net:7420'));
      expect(l.code, 'GOODCODE');
      expect(l.fingerprint, 'sha256:${'AB' * 32}');
      expect(l.name, 'home-server');
      expect(needsCode(_link), isFalse);
    });

    test('a bare URL needs the code, and gets https://', () {
      expect(needsCode('home.example.net:7420'), isTrue);
      final l = parseServerInput('home.example.net:7420', code: ' k7qx9m2p ');
      expect(l.url, Uri.parse('https://home.example.net:7420'));
      expect(l.code, 'K7QX9M2P');
      expect(l.fingerprint, isNull);
      expect(
        () => parseServerInput('https://home.example.net:7420'),
        throwsA(
          isA<FormatException>().having(
            (e) => e.message,
            'message',
            contains('pairing code'),
          ),
        ),
      );
      expect(() => parseServerInput(''), throwsFormatException);
      expect(
        () => parseServerInput('ftp://x', code: 'A'),
        throwsFormatException,
      );
    });
  });

  group('profiles', () {
    test('the connection survives a save; the token stays out of it', () async {
      final env = _Env();
      addTearDown(env.dispose);
      env.prefs.connection = RemoteProfile(
        url: Uri.parse('https://home.example.net:7420'),
        name: 'home-server',
        pin: 'sha256:${'AB' * 32}',
        deviceId: 'dev1',
        scope: 'chat',
      );
      await env.prefs.save();
      final text = env.prefs.file.readAsStringSync();
      expect(text, isNot(contains('tern_dev')));
      final back = AppPrefs.fromFile(env.prefs.file).connection;
      expect(back, isA<RemoteProfile>());
      back as RemoteProfile;
      expect(back.name, 'home-server');
      expect(back.scope, 'chat');
      expect(back.pin, 'sha256:${'AB' * 32}');

      env.prefs.connection = const LocalProfile();
      await env.prefs.save();
      expect(AppPrefs.fromFile(env.prefs.file).connection, isA<LocalProfile>());
      expect(AppPrefs.fromFile(File('/nonexistent')).connection, isNull);
    });

    test('endpointFor needs the token for a server', () async {
      final tokens = _MemTokens();
      final p = RemoteProfile(
        url: Uri.parse('https://h:7420'),
        name: 'h',
        pin: 'sha256:${'CD' * 32}',
      );
      expect(await endpointFor(null, tokens), isNull);
      expect(
        await endpointFor(const LocalProfile(), tokens),
        isA<UnixEndpoint>(),
      );
      // Token gone (another Mac, Keychain reset): ask again.
      expect(await endpointFor(p, tokens), isNull);
      tokens.tokens[p.account] = 'tern_dev_x';
      final e = await endpointFor(p, tokens) as RemoteEndpoint;
      expect(e.token, 'tern_dev_x');
      expect(e.pin, p.pin);
      expect(e.base, p.url);
    });

    test('the Keychain is reached through `security`', () async {
      final calls = <List<String>>[];
      final k = KeychainTokens(
        run: (args) async {
          calls.add(args);
          return ProcessResult(0, 0, 'tern_dev_x\n', '');
        },
      );
      await k.write('https://h:7420', 'tern_dev_x');
      expect(await k.read('https://h:7420'), 'tern_dev_x');
      await k.delete('https://h:7420');
      const s = KeychainTokens.service;
      expect(calls, [
        [
          'add-generic-password',
          '-U',
          '-s',
          s,
          '-a',
          'https://h:7420',
          '-w',
          'tern_dev_x',
        ],
        ['find-generic-password', '-s', s, '-a', 'https://h:7420', '-w'],
        ['delete-generic-password', '-s', s, '-a', 'https://h:7420'],
      ]);
    });

    test('connecting stores the token and switching back forgets it', () async {
      final env = _Env();
      addTearDown(env.dispose);
      final p = await env.connections.connect(parseServerInput(_link));
      expect(env.tokens.tokens, {p.account: 'tern_dev_secret'});
      expect(env.connections.isRemote, isTrue);
      expect((env.applied.single as RemoteEndpoint).token, 'tern_dev_secret');

      await env.connections.useLocal();
      expect(env.tokens.tokens, isEmpty);
      expect(env.applied.last, isA<UnixEndpoint>());
      expect(AppPrefs.fromFile(env.prefs.file).connection, isA<LocalProfile>());
    });
  });

  group('first launch', () {
    testWidgets('on this Mac, with Start at login', (tester) async {
      final env = _Env();
      addTearDown(env.dispose);
      late BuildContext ctx;
      await tester.pumpWidget(
        _app((c) {
          ctx = c;
          return const SizedBox();
        }),
      );
      final done = maybeAskWhereToRun(ctx, env.control, env.connections);
      await _settle(tester);
      expect(find.text('Where should tern run?'), findsOneWidget);
      expect(find.textContaining('daemon'), findsNothing);
      // tern Cloud has its place but can't be picked yet.
      final cloud = tester.widget<RadioListTile<Object?>>(
        find.ancestor(
          of: find.text(CloudOffer.title),
          matching: find.byWidgetPredicate((w) => w is RadioListTile),
        ),
      );
      expect(cloud.enabled, CloudOffer.available);

      await tester.tap(find.text('Continue'));
      await _settle(tester);
      await tester.runAsync(() => done);
      expect(find.text('Where should tern run?'), findsNothing);
      expect(env.runs.last, [
        'service',
        'install',
        if (Platform.isMacOS) '--tray',
      ]);
      expect(env.prefs.connection, isA<LocalProfile>());
      // Already on this computer: nothing to switch.
      expect(env.applied, isEmpty);

      // Answered: not asked again.
      env.runs.clear();
      await tester.runAsync(
        () => maybeAskWhereToRun(ctx, env.control, env.connections),
      );
      await tester.pumpAndSettle();
      expect(find.text('Where should tern run?'), findsNothing);
      expect(env.runs, isEmpty);
    });

    testWidgets('on a server: no tray, no service, the token is kept', (
      tester,
    ) async {
      final env = _Env();
      addTearDown(env.dispose);
      late BuildContext ctx;
      await tester.pumpWidget(
        _app((c) {
          ctx = c;
          return const SizedBox();
        }),
      );
      final done = maybeAskWhereToRun(ctx, env.control, env.connections);
      await _settle(tester);
      await tester.tap(find.text('On a tern server'));
      await tester.pumpAndSettle();
      expect(find.text('Start tern when I log in'), findsNothing);

      // A bare URL asks for the code; a wrong one says what to do.
      await tester.enterText(find.byType(TextField), 'home.example.net:7420');
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField).last, 'nope');
      await tester.tap(find.text('Continue'));
      await _settle(tester);
      expect(find.textContaining("didn't accept that code"), findsOneWidget);
      expect(env.prefs.connection, isNull);

      await tester.enterText(find.byType(TextField).first, _link);
      await tester.pumpAndSettle();
      expect(find.byType(TextField), findsOneWidget);
      await tester.tap(find.text('Continue'));
      await _settle(tester);
      await tester.runAsync(() => done);
      expect(find.text('Where should tern run?'), findsNothing);

      final p = env.prefs.connection as RemoteProfile;
      expect(p.name, 'home-server');
      expect(env.tokens.tokens[p.account], 'tern_dev_secret');
      expect(env.applied.single, isA<RemoteEndpoint>());
      // Nothing local was installed: the tray watches only this Mac.
      expect(env.runs.where((r) => r.contains('install')), isEmpty);
    });

    testWidgets('not asked when tern already starts at login here', (
      tester,
    ) async {
      final env = _Env(statusJson: ['{"at_login":true}']);
      addTearDown(env.dispose);
      late BuildContext ctx;
      await tester.pumpWidget(
        _app((c) {
          ctx = c;
          return const SizedBox();
        }),
      );
      await tester.runAsync(
        () => maybeAskWhereToRun(ctx, env.control, env.connections),
      );
      await tester.pumpAndSettle();
      expect(find.text('Where should tern run?'), findsNothing);
      expect(env.prefs.connection, isA<LocalProfile>());
    });
  });

  testWidgets('settings on a server: no local controls, a way back', (
    tester,
  ) async {
    final env = _Env();
    addTearDown(env.dispose);
    await tester.runAsync(
      () => env.connections.connect(parseServerInput(_link)),
    );
    final store = TernStore(TernClient(const Endpoint.unix('/nonexistent')));
    addTearDown(store.dispose);
    await tester.pumpWidget(
      _app(
        (_) => SettingsDialog(
          control: env.control,
          prefs: env.prefs,
          store: store,
          connections: env.connections,
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('tern runs on home-server'), findsOneWidget);
    expect(
      find.textContaining('https://home.example.net:7420'),
      findsOneWidget,
    );
    expect(find.text('Start tern when I log in'), findsNothing);
    expect(find.text('Quit tern completely'), findsNothing);
    expect(find.text('Change server…'), findsOneWidget);
    // It never asked the local service anything.
    expect(env.runs, isEmpty);

    await tester.tap(find.textContaining('instead'));
    await _settle(tester);
    expect(env.prefs.connection, isA<LocalProfile>());
    expect(env.tokens.tokens, isEmpty);
    expect(env.applied.last, isA<UnixEndpoint>());
  });
}
