import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/account.dart';
import 'package:tern_desktop/state/connection.dart';
import 'package:tern_desktop/state/daemon.dart';
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

String _b64(Object o) =>
    base64Url.encode(utf8.encode(jsonEncode(o))).replaceAll('=', '');

/// An unsigned JWT: the app only reads its claims.
String _jwt(DateTime exp, {String sub = 'user_1', String? email}) =>
    '${_b64({'alg': 'none'})}.${_b64({
      'sub': sub,
      'exp': exp.millisecondsSinceEpoch ~/ 1000,
      'email': ?email,
    })}.sig';

// `pk_test_` + base64("clerk.example.accounts.dev$")
const _pk = 'pk_test_Y2xlcmsuZXhhbXBsZS5hY2NvdW50cy5kZXYk';

const _config = ClerkConfig(publishableKey: _pk, clientId: 'client_abc');

/// A fake Clerk: records the forms it is sent, answers with fresh tokens.
class _Clerk {
  final posts = <(Uri, Map<String, String>)>[];
  var counter = 0;
  var refuseRefresh = false;
  DateTime expiry = DateTime.now().add(const Duration(hours: 1));

  Future<Map<String, dynamic>> post(Uri url, Map<String, String> form) async {
    posts.add((url, form));
    if (url.path == '/oauth/token/revoke') return {};
    if (form['grant_type'] == 'refresh_token' && refuseRefresh) {
      throw const AccountException('revoked', permanent: true);
    }
    counter++;
    return {
      'id_token': _jwt(expiry, email: 'mitch@example.com'),
      'access_token': 'opaque$counter',
      'refresh_token': 'refresh$counter',
    };
  }

  /// "The browser": follows the authorize URL's redirect with a code.
  Future<void> open(Uri authorize) async {
    final q = authorize.queryParameters;
    final back = Uri.parse(
      q['redirect_uri']!,
    ).replace(queryParameters: {'code': 'thecode', 'state': q['state']!});
    unawaited(
      HttpClient().getUrl(back).then((r) => r.close()).then((_) {}),
    );
  }
}

void main() {
  group('ClerkConfig', () {
    test('the issuer comes from the publishable key', () {
      expect(_config.issuer, Uri.parse('https://clerk.example.accounts.dev'));
    });

    test('a key that is not one means no accounts', () {
      expect(
        const ClerkConfig(publishableKey: 'nope', clientId: 'x').issuer,
        isNull,
      );
      expect(
        const ClerkConfig(publishableKey: 'pk_test_!!!', clientId: 'x').issuer,
        isNull,
      );
      // Nothing configured (no dart-define, empty environment).
      expect(ClerkConfig.fromEnvironment({}), isNull);
      expect(
        ClerkConfig.fromEnvironment({
          'TERN_CLERK_PUBLISHABLE_KEY': _pk,
          'TERN_CLERK_CLIENT_ID': 'client_abc',
        })?.clientId,
        'client_abc',
      );
    });
  });

  group('AccountService', () {
    late _Clerk clerk;
    late _MemTokens store;
    late AccountService service;

    setUp(() {
      clerk = _Clerk();
      store = _MemTokens();
      service = AccountService(
        config: _config,
        tokens: store,
        post: clerk.post,
        open: clerk.open,
      );
    });

    tearDown(() => service.dispose());

    test('sign in uses PKCE and keeps the session in the store', () async {
      await service.signIn();
      expect(service.signedIn, isTrue);
      expect(service.email, 'mitch@example.com');
      expect(service.userId, 'user_1');
      final (url, form) = clerk.posts.single;
      expect(url.path, '/oauth/token');
      expect(form['grant_type'], 'authorization_code');
      expect(form['code'], 'thecode');
      expect(form['client_id'], 'client_abc');
      expect(form['code_verifier'], isNotEmpty);
      expect(form['redirect_uri'], startsWith('http://127.0.0.1:'));
      expect(store.tokens[accountSessionKey], contains('refresh1'));
    });

    test('a different state is refused', () async {
      final bad = AccountService(
        config: _config,
        tokens: store,
        post: clerk.post,
        open: (url) async {
          final back = Uri.parse(url.queryParameters['redirect_uri']!)
              .replace(queryParameters: {'code': 'c', 'state': 'forged'});
          unawaited(HttpClient().getUrl(back).then((r) => r.close()));
        },
      );
      addTearDown(bad.dispose);
      await expectLater(bad.signIn(), throwsA(isA<AccountException>()));
      expect(bad.signedIn, isFalse);
      expect(clerk.posts, isEmpty);
    });

    test('a saved session loads, and an expiring one is renewed', () async {
      await service.signIn();
      clerk.posts.clear();
      // Saved as expiring in ten seconds.
      clerk.expiry = DateTime.now().add(const Duration(hours: 2));
      final soon = DateTime.now().add(const Duration(seconds: 10));
      store.tokens[accountSessionKey] = jsonEncode(
        AccountSession(
          bearer: _jwt(soon),
          refreshToken: 'refresh1',
          expiresAt: soon,
          userId: 'user_1',
          email: 'mitch@example.com',
        ).toJson(),
      );
      final again = AccountService(
        config: _config,
        tokens: store,
        post: clerk.post,
        open: clerk.open,
      );
      addTearDown(again.dispose);
      await again.load();
      final (_, form) = clerk.posts.single;
      expect(form['grant_type'], 'refresh_token');
      expect(form['refresh_token'], 'refresh1');
      expect(again.signedIn, isTrue);
      expect(jwtClaims(again.currentToken!)['exp'], greaterThan(
        soon.millisecondsSinceEpoch ~/ 1000 + 3000,
      ));
    });

    test('a token with time left is not renewed', () async {
      await service.signIn();
      clerk.posts.clear();
      expect(await service.freshToken(), service.currentToken);
      expect(clerk.posts, isEmpty);
    });

    test('a refused refresh signs out', () async {
      await service.signIn();
      clerk.refuseRefresh = true;
      await expectLater(service.renew(), throwsA(isA<AccountException>()));
      expect(service.signedIn, isFalse);
      expect(store.tokens, isNot(contains(accountSessionKey)));
    });

    test('sign out forgets the session and revokes the refresh token', () async {
      await service.signIn();
      await service.signOut();
      expect(service.signedIn, isFalse);
      expect(store.tokens, isEmpty);
      expect(clerk.posts.last.$1.path, '/oauth/token/revoke');
      expect(clerk.posts.last.$2['token'], 'refresh1');
    });

    test('AccountTokens answers the account key with the live token', () async {
      await service.signIn();
      final t = AccountTokens(store, service);
      expect(await t.read(accountTokenKey), service.currentToken);
      await t.write(accountTokenKey, 'ignored');
      await t.write('server:a', 'tern_dev_x');
      expect(await t.read('server:a'), 'tern_dev_x');
      expect(store.tokens, isNot(contains(accountTokenKey)));
    });
  });

  group('screens', () {
    late Directory dir;
    late AppPrefs prefs;
    late _MemTokens store;
    late AccountService service;
    late Connections connections;
    late TernControl control;
    final applied = <Endpoint>[];

    setUp(() {
      dir = Directory.systemTemp.createTempSync('tern_account');
      prefs = AppPrefs.fromFile(File('${dir.path}/desktop.json'));
      store = _MemTokens();
      applied.clear();
      control = TernControl(
        bin: '/fake/tern',
        env: {'HOME': dir.path},
        run: (bin, args) async => ProcessResult(0, 0, '', ''),
      );
    });

    tearDown(() => dir.deleteSync(recursive: true));

    Connections build({bool withAccount = true}) {
      service = AccountService(
        config: _config,
        tokens: store,
        post: _Clerk().post,
      );
      return connections = Connections(
        prefs: prefs,
        tokens: AccountTokens(store, service),
        apply: applied.add,
        account: withAccount ? service : null,
      );
    }

    Widget app(Widget Function(BuildContext) child) => MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(body: Builder(builder: child)),
    );

    testWidgets('first launch offers Sign in only with a Clerk key', (
      tester,
    ) async {
      await tester.pumpWidget(
        app(
          (_) => WhereToRunDialog(
            control: control,
            connections: build(withAccount: false),
          ),
        ),
      );
      expect(find.text('Sign in'), findsNothing);
      expect(find.text(CloudOffer.title), findsOneWidget);

      await tester.pumpWidget(
        app(
          (_) => WhereToRunDialog(control: control, connections: build()),
        ),
      );
      await tester.pumpAndSettle();
      // The third choice, after this computer and a server.
      final titles = tester
          .widgetList<RadioListTile<Object?>>(
            find.byWidgetPredicate((w) => w is RadioListTile),
          )
          .map((w) => (w.title! as Text).data)
          .toList();
      expect(titles.last, 'Sign in');
      expect(titles, hasLength(3));
      expect(find.text(CloudOffer.title), findsNothing);

      await tester.tap(find.text('Sign in'));
      await tester.pumpAndSettle();
      expect(find.text('Server address'), findsOneWidget);
    });

    testWidgets('Settings shows who is signed in, and Sign out', (
      tester,
    ) async {
      final c = build();
      // Signed out: no row.
      await tester.pumpWidget(
        app(
          (_) => SettingsDialog(control: control, prefs: prefs, connections: c),
        ),
      );
      expect(find.textContaining('Signed in as'), findsNothing);

      // A saved session, as if just signed in.
      await tester.runAsync(() async {
        final soon = DateTime.now().add(const Duration(hours: 1));
        store.tokens[accountSessionKey] = jsonEncode(
          AccountSession(
            bearer: _jwt(soon),
            refreshToken: 'r',
            expiresAt: soon,
            userId: 'user_1',
            email: 'mitch@example.com',
          ).toJson(),
        );
        await service.load();
      });
      await tester.pumpWidget(
        app(
          (_) => SettingsDialog(control: control, prefs: prefs, connections: c),
        ),
      );
      await tester.pump();
      expect(find.text('Signed in as mitch@example.com'), findsOneWidget);

      await tester.tap(find.text('Sign out'));
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 50)),
      );
      await tester.pumpAndSettle();
      expect(service.signedIn, isFalse);
      expect(find.textContaining('Signed in as'), findsNothing);
      expect(store.tokens, isNot(contains(accountSessionKey)));
    });
  });
}
