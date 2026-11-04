import 'dart:io';

import 'package:tern_client/tern_client.dart';

import 'daemon.dart';

/// Where the app's tern runs (deployment spec §7.1's connection profiles,
/// one at a time for now).
sealed class Profile {
  const Profile();

  static Profile? fromJson(Object? j) {
    if (j is! Map<String, dynamic>) return null;
    return switch (j['kind']) {
      'local' => const LocalProfile(),
      'remote' => RemoteProfile.fromJson(j),
      _ => null,
    };
  }

  Map<String, Object?> toJson();
}

/// tern on this computer, over its Unix socket.
final class LocalProfile extends Profile {
  const LocalProfile();

  @override
  Map<String, Object?> toJson() => {'kind': 'local'};
}

/// A tern server this app paired with. Its device token is in the
/// Keychain ([TokenStore]), never in `desktop.json`.
final class RemoteProfile extends Profile {
  const RemoteProfile({
    required this.url,
    required this.name,
    this.pin,
    this.deviceId,
    this.scope,
  });

  static RemoteProfile? fromJson(Map<String, dynamic> j) {
    final url = Uri.tryParse(j['url'] as String? ?? '');
    if (url == null || !url.hasScheme) return null;
    return RemoteProfile(
      url: url,
      name: j['name'] as String? ?? url.host,
      pin: j['pin'] as String?,
      deviceId: j['device_id'] as String?,
      scope: j['scope'] as String?,
    );
  }

  final Uri url;
  final String name;

  /// The certificate pin from the pairing link, if any.
  final String? pin;
  final String? deviceId;
  final String? scope;

  /// The Keychain account its token is stored under.
  String get account => url.toString();

  @override
  Map<String, Object?> toJson() => {
    'kind': 'remote',
    'url': url.toString(),
    'name': name,
    'pin': ?pin,
    'device_id': ?deviceId,
    'scope': ?scope,
  };
}

/// The future hosted option. One place to change when tern Cloud ships:
/// the chooser shows it disabled while [available] is false.
class CloudOffer {
  static const available = false;
  static const title = 'tern Cloud';
  static const subtitle = 'Coming soon: tern hosted for you, nothing to run.';
}

/// What someone typed in "Connect to a tern server": a pairing link
/// (`tern://pair?…`), or a server URL plus the pairing code from
/// `tern devices pair`. A URL without a scheme gets `https://`.
PairingLink parseServerInput(String input, {String? code}) {
  final text = input.trim();
  if (text.startsWith('tern://')) return PairingLink.parse(text);
  if (text.isEmpty) {
    throw const FormatException('Paste a pairing link or a server URL.');
  }
  final url = Uri.tryParse(text.contains('://') ? text : 'https://$text');
  if (url == null ||
      (url.scheme != 'https' && url.scheme != 'http') ||
      url.host.isEmpty) {
    throw FormatException("That doesn't look like a server URL.", text);
  }
  final c = code?.trim().toUpperCase() ?? '';
  if (c.isEmpty) {
    throw const FormatException(
      'Enter the pairing code from `tern devices pair` on the server.',
    );
  }
  return PairingLink(url: url, code: c);
}

/// Whether [input] needs a separate pairing code (it isn't a full link).
bool needsCode(String input) => !input.trim().startsWith('tern://');

/// Device tokens, in the macOS Keychain through the `security` tool.
/// Elsewhere (no Keychain) in a 0600 file beside `desktop.json`.
abstract interface class TokenStore {
  Future<String?> read(String account);
  Future<void> write(String account, String token);
  Future<void> delete(String account);

  static TokenStore platform([Map<String, String>? env]) => Platform.isMacOS
      ? KeychainTokens()
      : FileTokens(File('${AppPrefs.dir(env)}/tokens'));
}

/// Runs `security <args>`; replaced in tests.
typedef RunSecurity = Future<ProcessResult> Function(List<String> args);

Future<ProcessResult> _security(List<String> args) =>
    Process.run('/usr/bin/security', args);

class KeychainTokens implements TokenStore {
  KeychainTokens({this.run = _security});

  static const service = 'dev.tern.ternDesktop.device-token';
  final RunSecurity run;

  @override
  Future<String?> read(String account) async {
    final r = await run([
      'find-generic-password',
      '-s',
      service,
      '-a',
      account,
      '-w',
    ]);
    if (r.exitCode != 0) return null;
    final t = '${r.stdout}'.trim();
    return t.isEmpty ? null : t;
  }

  @override
  Future<void> write(String account, String token) async {
    final r = await run([
      'add-generic-password',
      '-U',
      '-s',
      service,
      '-a',
      account,
      '-w',
      token,
    ]);
    if (r.exitCode != 0) {
      throw TernCommandException(
        "Couldn't save the server's token in the Keychain: "
        '${'${r.stderr}'.trim()}',
      );
    }
  }

  @override
  Future<void> delete(String account) async {
    await run(['delete-generic-password', '-s', service, '-a', account]);
  }
}

class FileTokens implements TokenStore {
  FileTokens(this.file);

  final File file;

  Map<String, String> _all() {
    try {
      return {
        for (final l in file.readAsLinesSync())
          if (l.indexOf('\t') case final i when i > 0)
            l.substring(0, i): l.substring(i + 1),
      };
    } on Object {
      return {};
    }
  }

  Future<void> _save(Map<String, String> all) async {
    await file.parent.create(recursive: true);
    await file.writeAsString(
      all.entries.map((e) => '${e.key}\t${e.value}\n').join(),
    );
    await Process.run('chmod', ['600', file.path]);
  }

  @override
  Future<String?> read(String account) async => _all()[account];

  @override
  Future<void> write(String account, String token) =>
      _save(_all()..[account] = token);

  @override
  Future<void> delete(String account) => _save(_all()..remove(account));
}

/// The endpoint for [profile], or null when it can't be used (never
/// chosen, or a server whose token is gone): the app then asks again.
Future<Endpoint?> endpointFor(Profile? profile, TokenStore tokens) async {
  switch (profile) {
    case null:
      return null;
    case LocalProfile():
      return Endpoint.local();
    case RemoteProfile p:
      final token = await tokens.read(p.account);
      if (token == null) return null;
      return Endpoint.remote(p.url, token: token, pin: p.pin);
  }
}

/// Redeems a pairing link; replaced in tests.
typedef Pair = Future<Pairing> Function(PairingLink link);

Future<Pairing> _pair(PairingLink link) => TernClient.pair(
  link,
  deviceName: Platform.localHostname,
  platform: Platform.operatingSystem,
);

/// Switching where tern runs: keeps [AppPrefs.connection] and the token
/// in step, and hands the new endpoint to [apply] (which rebuilds the
/// store).
class Connections {
  Connections({
    required this.prefs,
    required this.tokens,
    required this.apply,
    this.pair = _pair,
  });

  final AppPrefs prefs;
  final TokenStore tokens;
  final void Function(Endpoint) apply;
  final Pair pair;

  Profile? get current => prefs.connection;

  bool get isRemote => current is RemoteProfile;

  /// Run tern on this computer; forgets a server's token. The app was
  /// already on this computer unless it was on a server.
  Future<void> useLocal() async {
    final old = current;
    if (old is RemoteProfile) await tokens.delete(old.account);
    prefs.connection = const LocalProfile();
    await prefs.save();
    if (old is RemoteProfile) apply(Endpoint.local());
  }

  /// Pair with a server and switch to it. Throws [TernException] (wrong
  /// or expired code, unreachable, certificate mismatch) or
  /// [TernCommandException] (Keychain) with nothing changed.
  Future<RemoteProfile> connect(PairingLink link) async {
    final p = await pair(link);
    final profile = RemoteProfile(
      url: link.url,
      name: link.name ?? link.url.host,
      pin: link.fingerprint,
      deviceId: p.deviceId,
      scope: p.scope,
    );
    await tokens.write(profile.account, p.endpoint.token);
    if (current case RemoteProfile old when old.account != profile.account) {
      await tokens.delete(old.account);
    }
    prefs.connection = profile;
    await prefs.save();
    apply(p.endpoint);
    return profile;
  }
}
