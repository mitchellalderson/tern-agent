import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:crypto/crypto.dart';
import 'package:flutter/foundation.dart';

import 'connection.dart';

/// Signing in to a hosted tern account (deployment spec §10.4a): the system
/// browser against Clerk's OAuth 2.0 provider, with PKCE and a loopback
/// redirect. No account is ever needed to run tern on this computer.

/// Where the tokens are kept in the Keychain ([TokenStore]).
const accountSessionKey = 'account:clerk:session';

/// What a saved server uses as its token account when it takes the
/// signed-in account instead of a device token.
const accountTokenKey = 'account:clerk';

/// The Clerk application this build signs in to. Absent (null) means the
/// build has no accounts, and the app doesn't mention them.
class ClerkConfig {
  const ClerkConfig({required this.publishableKey, required this.clientId});

  /// From `--dart-define=TERN_CLERK_PUBLISHABLE_KEY=…` and
  /// `--dart-define=TERN_CLERK_CLIENT_ID=…`, else the same names in the
  /// environment. Null unless both are set and the key is a real one.
  static ClerkConfig? fromEnvironment([Map<String, String>? env]) {
    env ??= Platform.environment;
    const pk = String.fromEnvironment('TERN_CLERK_PUBLISHABLE_KEY');
    const id = String.fromEnvironment('TERN_CLERK_CLIENT_ID');
    final c = ClerkConfig(
      publishableKey: pk.isNotEmpty
          ? pk
          : env['TERN_CLERK_PUBLISHABLE_KEY'] ?? '',
      clientId: id.isNotEmpty ? id : env['TERN_CLERK_CLIENT_ID'] ?? '',
    );
    return c.issuer == null || c.clientId.isEmpty ? null : c;
  }

  final String publishableKey;

  /// The OAuth application's client id (a public client: no secret).
  final String clientId;

  /// Clerk's Frontend API, which a publishable key spells out:
  /// `pk_test_` + base64 of `<host>$`.
  Uri? get issuer {
    final m = RegExp(r'^pk_(?:test|live)_(.+)$').firstMatch(publishableKey);
    if (m == null) return null;
    try {
      final host = utf8.decode(base64.decode(base64.normalize(m[1]!)));
      if (!host.endsWith(r'$') || host.length < 4) return null;
      return Uri.https(host.substring(0, host.length - 1));
    } on FormatException {
      return null;
    }
  }
}

/// What a sign-in leaves: the token tern servers are shown, and what
/// renews it.
class AccountSession {
  const AccountSession({
    required this.bearer,
    required this.refreshToken,
    required this.expiresAt,
    required this.userId,
    this.email,
  });

  static AccountSession? fromJson(Object? j) {
    if (j is! Map<String, dynamic>) return null;
    final bearer = j['bearer'], refresh = j['refresh'], user = j['user'];
    final exp = j['exp'];
    if (bearer is! String || user is! String || exp is! int) return null;
    return AccountSession(
      bearer: bearer,
      refreshToken: refresh is String ? refresh : null,
      expiresAt: DateTime.fromMillisecondsSinceEpoch(exp * 1000, isUtc: true),
      userId: user,
      email: j['email'] as String?,
    );
  }

  /// The OIDC ID token (a JWT whose audience is this app's client id): what
  /// a core verifies. Falls back to the access token if there is none.
  final String bearer;
  final String? refreshToken;
  final DateTime expiresAt;
  final String userId;
  final String? email;

  Map<String, Object?> toJson() => {
    'bearer': bearer,
    'refresh': ?refreshToken,
    'exp': expiresAt.millisecondsSinceEpoch ~/ 1000,
    'user': userId,
    'email': ?email,
  };
}

/// Posts a form to Clerk's token endpoint and returns the JSON answer.
/// Replaced in tests. Throws [AccountException].
typedef PostForm = Future<Map<String, dynamic>> Function(
  Uri url,
  Map<String, String> form,
);

/// Opens a page in the system browser. Replaced in tests.
typedef OpenUrl = Future<void> Function(Uri url);

class AccountException implements Exception {
  const AccountException(this.message, {this.permanent = false});

  final String message;

  /// The sign-in is over for good (the refresh token was refused), as
  /// opposed to a network failure that may pass.
  final bool permanent;

  @override
  String toString() => message;
}

Future<Map<String, dynamic>> _postForm(
  Uri url,
  Map<String, String> form,
) async {
  final client = HttpClient()..connectionTimeout = const Duration(seconds: 15);
  try {
    final req = await client.postUrl(url);
    req.headers.contentType = ContentType(
      'application',
      'x-www-form-urlencoded',
      charset: 'utf-8',
    );
    req.write(
      form.entries
          .map(
            (e) =>
                '${Uri.encodeQueryComponent(e.key)}=${Uri.encodeQueryComponent(e.value)}',
          )
          .join('&'),
    );
    final res = await req.close();
    final body = await res.transform(utf8.decoder).join();
    Map<String, dynamic> json;
    try {
      json = jsonDecode(body) as Map<String, dynamic>;
    } on Object {
      json = {};
    }
    if (res.statusCode != 200) {
      final bad = res.statusCode >= 400 && res.statusCode < 500;
      throw AccountException(
        json['error_description'] as String? ??
            "The sign-in service didn't accept that (${res.statusCode}).",
        permanent: bad,
      );
    }
    return json;
  } on SocketException catch (e) {
    throw AccountException("Couldn't reach the sign-in service: ${e.message}");
  } finally {
    client.close(force: true);
  }
}

Future<void> _openBrowser(Uri url) async {
  final (cmd, args) = Platform.isMacOS
      ? ('open', [url.toString()])
      : Platform.isWindows
      ? ('cmd', ['/c', 'start', '', url.toString()])
      : ('xdg-open', [url.toString()]);
  final r = await Process.run(cmd, args);
  if (r.exitCode != 0) {
    throw const AccountException("Couldn't open your browser.");
  }
}

String _b64url(List<int> bytes) => base64Url.encode(bytes).replaceAll('=', '');

String _random(int bytes) {
  final r = Random.secure();
  return _b64url([for (var i = 0; i < bytes; i++) r.nextInt(256)]);
}

/// The claims of a JWT, unchecked: for showing who is signed in. Cores
/// verify the signature; the app only trusts what Clerk's endpoint sent.
Map<String, dynamic> jwtClaims(String jwt) {
  final parts = jwt.split('.');
  if (parts.length != 3) return {};
  try {
    final j = jsonDecode(
      utf8.decode(base64Url.decode(base64Url.normalize(parts[1]))),
    );
    return j is Map<String, dynamic> ? j : {};
  } on Object {
    return {};
  }
}

/// The signed-in account: sign in, stay signed in (renewing the token
/// before it expires), sign out. Tokens live in the Keychain via [tokens].
class AccountService extends ChangeNotifier {
  AccountService({
    required this.config,
    required this.tokens,
    PostForm? post,
    OpenUrl? open,
    DateTime Function()? now,
    this.renewBefore = const Duration(minutes: 2),
    this.signInTimeout = const Duration(minutes: 5),
  }) : _post = post ?? _postForm,
       _open = open ?? _openBrowser,
       _now = now ?? DateTime.now;

  final ClerkConfig config;
  final TokenStore tokens;
  final PostForm _post;
  final OpenUrl _open;
  final DateTime Function() _now;
  final Duration renewBefore;
  final Duration signInTimeout;

  AccountSession? _session;
  Timer? _timer;
  Future<void>? _renewing;

  bool get signedIn => _session != null;
  String? get email => _session?.email;
  String? get userId => _session?.userId;

  /// The token to send now, as it was last renewed (no waiting).
  String? get currentToken => _session?.bearer;

  Uri get _issuer => config.issuer!;

  /// Reads a saved session, renewing it if it is about to expire.
  Future<void> load() async {
    final raw = await tokens.read(accountSessionKey);
    if (raw == null) return;
    try {
      _session = AccountSession.fromJson(jsonDecode(raw));
    } on FormatException {
      _session = null;
    }
    if (_session == null) return;
    await freshToken();
    _schedule();
  }

  /// A token with time left, renewing first if needed. Null when signed
  /// out (including when renewal is refused).
  Future<String?> freshToken() async {
    final s = _session;
    if (s == null) return null;
    if (s.expiresAt.difference(_now()) > const Duration(seconds: 30)) {
      return s.bearer;
    }
    try {
      await renew();
    } on AccountException {
      // Offline: the old token is all there is, and the server will say.
    }
    return _session?.bearer;
  }

  /// Opens the browser to sign in and waits for the answer. Throws
  /// [AccountException] if cancelled, refused or timed out.
  Future<void> signIn() async {
    final verifier = _random(32);
    final state = _random(16);
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    try {
      final redirect = 'http://127.0.0.1:${server.port}/callback';
      final authorize = _issuer.replace(
        path: '/oauth/authorize',
        queryParameters: {
          'response_type': 'code',
          'client_id': config.clientId,
          'redirect_uri': redirect,
          'scope': 'openid profile email offline_access',
          'state': state,
          'code_challenge': _b64url(
            sha256.convert(utf8.encode(verifier)).bytes,
          ),
          'code_challenge_method': 'S256',
        },
      );
      final answer = server
          .firstWhere((r) => r.uri.path == '/callback')
          .timeout(
            signInTimeout,
            onTimeout: () =>
                throw const AccountException('Sign-in timed out. Try again.'),
          );
      // If opening the browser fails, nobody awaits this any more.
      answer.ignore();
      await _open(authorize);
      final req = await answer;
      final q = req.uri.queryParameters;
      final ok = q['state'] == state && q['code'] != null;
      req.response
        ..headers.contentType = ContentType.html
        ..write(
          ok
              ? '<!doctype html><title>tern</title><p style="font:16px sans-serif">'
                    "You're signed in. You can close this tab.</p>"
              : '<!doctype html><title>tern</title><p style="font:16px sans-serif">'
                    "Sign-in didn't finish. Go back to tern and try again.</p>",
        );
      await req.response.close();
      if (q['state'] != state) {
        throw const AccountException('Sign-in was not started by this app.');
      }
      if (q['error'] != null || q['code'] == null) {
        throw AccountException(
          q['error_description'] ?? 'Sign-in was cancelled.',
        );
      }
      final json = await _post(_issuer.replace(path: '/oauth/token'), {
        'grant_type': 'authorization_code',
        'code': q['code']!,
        'redirect_uri': redirect,
        'client_id': config.clientId,
        'code_verifier': verifier,
      });
      await _adopt(json, previous: null);
    } finally {
      await server.close(force: true);
    }
  }

  /// Gets a new token with the refresh token.
  Future<void> renew() =>
      _renewing ??= _renew().whenComplete(() => _renewing = null);

  Future<void> _renew() async {
    final s = _session;
    final refresh = s?.refreshToken;
    if (s == null) return;
    if (refresh == null) {
      await signOut();
      return;
    }
    try {
      final json = await _post(_issuer.replace(path: '/oauth/token'), {
        'grant_type': 'refresh_token',
        'refresh_token': refresh,
        'client_id': config.clientId,
      });
      await _adopt(json, previous: s);
    } on AccountException catch (e) {
      if (e.permanent) {
        // Revoked or expired for good: signed out.
        await signOut();
      }
      rethrow;
    }
  }

  Future<void> _adopt(
    Map<String, dynamic> json, {
    required AccountSession? previous,
  }) async {
    final id = json['id_token'] as String?;
    final access = json['access_token'] as String?;
    final bearer = id ?? access;
    if (bearer == null) {
      throw const AccountException('The sign-in service sent no token.');
    }
    final claims = jwtClaims(bearer);
    final user = (claims['sub'] as String?) ?? previous?.userId;
    if (user == null) {
      throw const AccountException('The sign-in service sent no account.');
    }
    final exp = claims['exp'] is int
        ? DateTime.fromMillisecondsSinceEpoch(
            (claims['exp'] as int) * 1000,
            isUtc: true,
          )
        : _now().add(Duration(seconds: (json['expires_in'] as int?) ?? 3600));
    final session = AccountSession(
      bearer: bearer,
      refreshToken:
          (json['refresh_token'] as String?) ?? previous?.refreshToken,
      expiresAt: exp,
      userId: user,
      email: (claims['email'] as String?) ?? previous?.email,
    );
    await tokens.write(accountSessionKey, jsonEncode(session.toJson()));
    _session = session;
    _schedule();
    notifyListeners();
  }

  void _schedule() {
    _timer?.cancel();
    final s = _session;
    if (s == null) return;
    var wait = s.expiresAt.difference(_now()) - renewBefore;
    if (wait < const Duration(seconds: 5)) wait = const Duration(seconds: 5);
    _timer = Timer(wait, () async {
      try {
        await renew();
      } on AccountException {
        // Try again soon unless that sign-in is over.
        if (_session != null) {
          _timer = Timer(const Duration(seconds: 30), _schedule);
        }
      }
    });
  }

  /// Forgets the account on this computer (and tells Clerk to drop the
  /// refresh token, best effort).
  Future<void> signOut() async {
    final s = _session;
    _session = null;
    _timer?.cancel();
    await tokens.delete(accountSessionKey);
    notifyListeners();
    final refresh = s?.refreshToken;
    if (refresh != null) {
      try {
        await _post(_issuer.replace(path: '/oauth/token/revoke'), {
          'token': refresh,
          'token_type_hint': 'refresh_token',
          'client_id': config.clientId,
        });
      } on Object {
        // Not worth stopping a sign-out for.
      }
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }
}

/// A [TokenStore] that answers [accountTokenKey] with the signed-in
/// account's current token, and everything else from [inner].
class AccountTokens implements TokenStore {
  AccountTokens(this.inner, this.account);

  final TokenStore inner;
  final AccountService account;

  @override
  Future<String?> read(String key) =>
      key == accountTokenKey ? account.freshToken() : inner.read(key);

  @override
  Future<void> write(String key, String token) async {
    if (key != accountTokenKey) await inner.write(key, token);
  }

  @override
  Future<void> delete(String key) async {
    if (key != accountTokenKey) await inner.delete(key);
  }
}
