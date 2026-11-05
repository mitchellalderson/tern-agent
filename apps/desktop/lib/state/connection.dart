import 'dart:async';
import 'dart:io';

import 'package:tern_client/tern_client.dart';

import 'daemon.dart';

/// Where the app's tern runs: one saved entry per place (deployment spec
/// §7.1's connection profiles). The built-in [LocalProfile] ("This Mac")
/// is always there and can't be removed; the user may save any number of
/// [RemoteProfile]s beside it and switch between them.
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

  /// Stable and never shown: what the app remembers a server by.
  String get id;

  Map<String, Object?> toJson();
}

/// What the built-in entry is called.
String get localName => Platform.isMacOS ? 'This Mac' : 'This computer';

/// tern on this computer, over its Unix socket.
final class LocalProfile extends Profile {
  const LocalProfile();

  static const localId = 'local';

  @override
  String get id => localId;

  @override
  Map<String, Object?> toJson() => {'kind': 'local'};
}

var _serverCounter = 0;

/// A new id for a saved server.
String newServerId() =>
    's${DateTime.now().microsecondsSinceEpoch.toRadixString(36)}'
    '${(_serverCounter++).toRadixString(36)}';

/// A tern server this app paired with. Its device token is in the
/// Keychain ([TokenStore]) under [account], never in `desktop.json`.
final class RemoteProfile extends Profile {
  const RemoteProfile._({
    required this.id,
    required this.account,
    required this.url,
    required this.name,
    this.pin,
    this.deviceId,
    this.scope,
  });

  /// A new saved server (or, with an [id] and [tokenAccount], a known one).
  factory RemoteProfile({
    required Uri url,
    required String name,
    String? pin,
    String? deviceId,
    String? scope,
    String? id,
    String? tokenAccount,
  }) {
    final i = id ?? newServerId();
    return RemoteProfile._(
      id: i,
      account: tokenAccount ?? 'server:$i',
      url: url,
      name: name,
      pin: pin,
      deviceId: deviceId,
      scope: scope,
    );
  }

  /// Reads an entry. One saved by a version that kept a single server has
  /// no id or account: its id comes from its address and its token stays
  /// where it was, under the URL, so the connection survives the upgrade.
  static RemoteProfile? fromJson(Map<String, dynamic> j) {
    final url = Uri.tryParse(j['url'] as String? ?? '');
    if (url == null || !url.hasScheme) return null;
    return RemoteProfile(
      url: url,
      name: j['name'] as String? ?? url.host,
      pin: j['pin'] as String?,
      deviceId: j['device_id'] as String?,
      scope: j['scope'] as String?,
      id:
          j['id'] as String? ??
          'srv-${url.toString().hashCode.toRadixString(36)}',
      tokenAccount: j['account'] as String? ?? url.toString(),
    );
  }

  @override
  final String id;
  final Uri url;
  final String name;

  /// The certificate pin from the pairing link, if any.
  final String? pin;
  final String? deviceId;
  final String? scope;

  /// The Keychain account its token is stored under. Each saved server has
  /// its own, so a token is only ever read to talk to its own server.
  final String account;

  /// The same entry with other details (its id and token account stay).
  RemoteProfile copyWith({
    Uri? url,
    String? name,
    String? pin,
    bool clearPin = false,
    String? deviceId,
    String? scope,
  }) => RemoteProfile._(
    id: id,
    account: account,
    url: url ?? this.url,
    name: name ?? this.name,
    pin: clearPin ? null : pin ?? this.pin,
    deviceId: deviceId ?? this.deviceId,
    scope: scope ?? this.scope,
  );

  /// Whether [other] is the same address (scheme, host and port).
  bool sameAddress(Uri other) =>
      url.scheme == other.scheme &&
      url.host.toLowerCase() == other.host.toLowerCase() &&
      url.port == other.port;

  @override
  Map<String, Object?> toJson() => {
    'kind': 'remote',
    'id': id,
    'url': url.toString(),
    'name': name,
    'account': account,
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
/// `tern devices pair`. A URL without a scheme gets `https://`. Throws
/// [FormatException] with a sentence to show, including for plain `http://`
/// to another computer (the token would travel unprotected).
PairingLink parseServerInput(String input, {String? code}) {
  final text = input.trim();
  final PairingLink link;
  if (text.startsWith('tern://')) {
    link = PairingLink.parse(text);
  } else {
    if (text.isEmpty) {
      throw const FormatException('Paste a pairing link or a server URL.');
    }
    final url = parseServerUrl(text);
    final c = code?.trim().toUpperCase() ?? '';
    if (c.isEmpty) {
      throw const FormatException(
        'Enter the pairing code from `tern devices pair` on the server.',
      );
    }
    link = PairingLink(url: url, code: c);
  }
  final problem = remoteUrlProblem(link.url);
  if (problem != null) throw FormatException(problem, link.url.toString());
  return link;
}

/// A server address as typed: `home.example.net:7420` gets `https://`.
/// Throws [FormatException] with a sentence to show.
Uri parseServerUrl(String input) {
  final text = input.trim();
  final url = Uri.tryParse(text.contains('://') ? text : 'https://$text');
  if (url == null ||
      (url.scheme != 'https' && url.scheme != 'http') ||
      url.host.isEmpty) {
    throw FormatException("That doesn't look like a server URL.", text);
  }
  final problem = remoteUrlProblem(url);
  if (problem != null) throw FormatException(problem, text);
  // `https://host/` and `https://host` are one address.
  return url.path == '/' ? url.replace(path: '') : url;
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
/// chosen, a server whose token is gone, or an address that isn't allowed
/// such as plain http to another computer): the app then asks again.
/// The token read is always the one saved for [profile] itself.
Future<Endpoint?> endpointFor(Profile? profile, TokenStore tokens) async {
  switch (profile) {
    case null:
      return null;
    case LocalProfile():
      return Endpoint.local();
    case RemoteProfile p:
      if (remoteUrlProblem(p.url) != null) return null;
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

/// Asks whether a tern server answers at an address, with no token;
/// replaced in tests.
typedef ProbeUrl = Future<void> Function(Uri url, {String? pin});

Future<void> _probeUrl(Uri url, {String? pin}) =>
    TernClient.probe(url, pin: pin);

/// How a saved server looks right now.
enum Reach {
  /// Not asked yet, or asking.
  checking,
  online,

  /// Didn't answer (off, asleep, not reachable from here).
  offline,

  /// Answered, but the saved token was refused (revoked), or it is gone.
  signedOut,

  /// Can't be used as saved (an address that isn't https).
  unusable,
}

class ServerStatus {
  const ServerStatus(this.reach, [this.message, this.version]);

  final Reach reach;

  /// A sentence for a person; null when online.
  final String? message;
  final String? version;

  static const checking = ServerStatus(Reach.checking);
}

/// Asks a saved server whether it's there, with its own token and no one
/// else's; replaced in tests.
typedef CheckServer = Future<ServerStatus> Function(Profile profile);

/// Checks [profile] with a request to its own address: this computer's
/// socket, or the server's `/v1/health` with the token saved for it.
Future<ServerStatus> checkServer(Profile profile, TokenStore tokens) async {
  final Endpoint? endpoint;
  switch (profile) {
    case LocalProfile():
      endpoint = Endpoint.local();
    case RemoteProfile p:
      final problem = remoteUrlProblem(p.url);
      if (problem != null) return ServerStatus(Reach.unusable, problem);
      final token = await tokens.read(p.account);
      if (token == null) {
        return const ServerStatus(
          Reach.signedOut,
          'This device is signed out of this server. Edit it to pair again.',
        );
      }
      endpoint = Endpoint.remote(p.url, token: token, pin: p.pin);
  }
  final client = TernClient(endpoint);
  try {
    final h = await client.health().timeout(
      const Duration(seconds: 6),
      onTimeout: () => throw const TernException("can't reach it: timed out"),
    );
    return ServerStatus(Reach.online, null, h.version);
  } on SignedOutException {
    return const ServerStatus(
      Reach.signedOut,
      'This server no longer accepts this device. Edit it to pair again.',
    );
  } on TernException catch (e) {
    return ServerStatus(
      Reach.offline,
      profile is LocalProfile
          ? "tern isn't running on this computer."
          : serverErrorText(e),
    );
  } finally {
    client.close();
  }
}

/// A user-facing sentence for a failure while connecting to a server.
String serverErrorText(Object e) => switch (e) {
  FormatException(:final message) => message,
  SignedOutException() =>
    'This server no longer accepts this device. Pair it again with a new '
        'code from `tern devices pair`.',
  TernException(status: 401) =>
    "The server didn't accept that code. Codes work once and expire after "
        '10 minutes; make a new one with `tern devices pair`.',
  TernException(status: 404) =>
    "Something answers at that address, but it doesn't look like a tern "
        'server. Check the address and the port.',
  TernException(status: final s?) when s >= 300 && s < 400 =>
    'That address sends you somewhere else. Use the server\'s own address '
        '(the one `tern devices pair` shows).',
  TernException e when e.message.contains("can't establish a secure") =>
    "Found the server, but its security certificate isn't the one this "
        'app expects. If the server was reinstalled, pair again with a new '
        "link; if you didn't expect that, don't connect.",
  TernException e when e.unreachable =>
    "Couldn't reach the server. Check the address, that it is running, and "
        "that you're online (or on its VPN). ($e)",
  TernCommandException(:final message) => message,
  _ => '$e',
};

/// Switching where tern runs, over any number of saved servers: keeps
/// [AppPrefs] and the Keychain in step, and hands the new endpoint to
/// [apply] (which rebuilds the store, and with it every stream, cache and
/// selection, so nothing of one server shows up under another).
class Connections {
  Connections({
    required this.prefs,
    required this.tokens,
    required this.apply,
    this.pair = _pair,
    this.probeUrl = _probeUrl,
    this.check,
  });

  final AppPrefs prefs;
  final TokenStore tokens;
  final void Function(Endpoint) apply;
  final Pair pair;
  final ProbeUrl probeUrl;
  final CheckServer? check;

  /// Runs one change at a time, so two quick clicks can't leave the saved
  /// choice and the connection on different servers.
  Future<void>? _tail;
  var _pending = 0;

  Future<T> _one<T>(Future<T> Function() f) {
    // Starts at once when nothing else is running.
    final prev = _pending == 0 ? null : _tail;
    _pending++;
    final done = prev == null ? f() : prev.then((_) => f());
    final settled = done.then<void>((_) {}, onError: (_) {});
    _tail = settled;
    settled.then((_) => _pending--);
    return done;
  }

  Profile? get current => prefs.connection;

  bool get isRemote => current is RemoteProfile;

  /// The saved servers, in the order they were added. This computer isn't
  /// in the list: it is always there.
  List<RemoteProfile> get servers => List.unmodifiable(prefs.servers);

  /// The id of the server in use ([LocalProfile.localId] before the first
  /// choice).
  String get activeId => current?.id ?? LocalProfile.localId;

  RemoteProfile? server(String id) =>
      prefs.servers.where((s) => s.id == id).firstOrNull;

  /// What [id] is called ("This Mac" for the built-in).
  String nameOf(String id) =>
      id == LocalProfile.localId ? localName : server(id)?.name ?? id;

  /// How [profile] looks right now (a request to its own address).
  Future<ServerStatus> status(Profile profile) =>
      (check ?? (p) => checkServer(p, tokens))(profile);

  /// Why [name] can't be a server's name, or null. [except] is the server
  /// being renamed.
  String? nameProblem(String name, {String? except}) {
    final n = name.trim();
    if (n.isEmpty) return 'Give it a name, like "Home server".';
    if (n.length > 40) return 'Keep the name under 40 characters.';
    final taken = [
      if (except != LocalProfile.localId) localName,
      for (final s in prefs.servers)
        if (s.id != except) s.name,
    ].any((x) => x.toLowerCase() == n.toLowerCase());
    return taken ? 'You already have a server called "$n".' : null;
  }

  String _freeName(String wanted) {
    var n = wanted.trim().isEmpty ? 'tern server' : wanted.trim();
    if (n.length > 40) n = n.substring(0, 40);
    var name = n;
    for (var i = 2; nameProblem(name) != null; i++) {
      name = '$n $i';
    }
    return name;
  }

  /// Whether a tern server answers at [url] (certificate pinned to [pin]
  /// when given). Sends no token. Throws [TernException].
  Future<void> test(Uri url, {String? pin}) {
    final problem = remoteUrlProblem(url);
    if (problem != null) throw FormatException(problem);
    return probeUrl(url, pin: pin);
  }

  /// Run tern on this computer. Saved servers and their tokens stay.
  Future<void> useLocal() => switchTo(LocalProfile.localId, force: false);

  /// Use the server [id] (or this computer). Throws [FormatException] with
  /// a sentence to show when it can't be used; nothing changes then.
  /// [force]: re-apply even if it's the one in use.
  Future<void> switchTo(String id, {bool force = false}) => _one(() async {
    final old = current;
    if (id == LocalProfile.localId) {
      if (old is LocalProfile && !force) return;
      prefs.connection = const LocalProfile();
      await prefs.save();
      // Already on this computer when nothing was chosen or it's a first
      // choice; otherwise the app shows another server now.
      if (old is RemoteProfile || force) apply(Endpoint.local());
      return;
    }
    final p = server(id);
    if (p == null) {
      throw const FormatException('That server is no longer saved.');
    }
    if (old?.id == id && !force) return;
    final problem = remoteUrlProblem(p.url);
    if (problem != null) throw FormatException(problem);
    final token = await tokens.read(p.account);
    if (token == null) {
      throw FormatException(
        'This device is signed out of ${p.name}. Edit it to pair again.',
      );
    }
    prefs.connection = p;
    await prefs.save();
    apply(Endpoint.remote(p.url, token: token, pin: p.pin));
  });

  /// Pair with a server and save it, then switch to it (unless
  /// [activate] is false). A server already saved at the same address is
  /// paired again in place (keeping its name) instead of listed twice.
  /// Throws [TernException] (wrong or expired code, unreachable,
  /// certificate mismatch), [FormatException] or [TernCommandException]
  /// (Keychain) with nothing changed.
  Future<RemoteProfile> connect(
    PairingLink link, {
    String? name,
    bool activate = true,
  }) => _one(() async {
    final problem = remoteUrlProblem(link.url);
    if (problem != null) throw FormatException(problem);
    final wanted = name?.trim();
    if (wanted != null && wanted.isNotEmpty) {
      final existing = prefs.servers
          .where((s) => s.sameAddress(link.url))
          .firstOrNull;
      final p = nameProblem(wanted, except: existing?.id);
      if (p != null) throw FormatException(p);
    }
    final paired = await pair(link);
    final existing = prefs.servers
        .where((s) => s.sameAddress(link.url))
        .firstOrNull;
    final profile =
        existing?.copyWith(
          url: link.url,
          name: wanted == null || wanted.isEmpty ? null : wanted,
          pin: link.fingerprint,
          clearPin: link.fingerprint == null,
          deviceId: paired.deviceId,
          scope: paired.scope,
        ) ??
        RemoteProfile(
          url: link.url,
          name: wanted == null || wanted.isEmpty
              ? _freeName(link.name ?? link.url.host)
              : wanted,
          pin: link.fingerprint,
          deviceId: paired.deviceId,
          scope: paired.scope,
        );
    await tokens.write(profile.account, paired.endpoint.token);
    _put(profile);
    final wasActive = current?.id == profile.id;
    if (activate || wasActive) prefs.connection = profile;
    await prefs.save();
    if (activate || wasActive) apply(paired.endpoint);
    return profile;
  });

  /// Pair [id] again, at its address or a new one. The old token is
  /// forgotten only once the new pairing worked, and a token is never
  /// sent anywhere by this: pairing sends the code.
  Future<RemoteProfile> repair(
    String id,
    PairingLink link, {
    String? name,
  }) => _one(() async {
    final old = server(id);
    if (old == null) {
      throw const FormatException('That server is no longer saved.');
    }
    final problem = remoteUrlProblem(link.url);
    if (problem != null) throw FormatException(problem);
    final wanted = (name ?? old.name).trim();
    final np = nameProblem(wanted, except: id);
    if (np != null) throw FormatException(np);
    if (prefs.servers.any((s) => s.id != id && s.sameAddress(link.url))) {
      throw FormatException(
        'That address is already saved as '
        '"${prefs.servers.firstWhere((s) => s.id != id && s.sameAddress(link.url)).name}".',
      );
    }
    final paired = await pair(link);
    final profile = old.copyWith(
      url: link.url,
      name: wanted,
      pin: link.fingerprint,
      clearPin: link.fingerprint == null,
      deviceId: paired.deviceId,
      scope: paired.scope,
    );
    await tokens.write(profile.account, paired.endpoint.token);
    _put(profile);
    final wasActive = current?.id == id;
    if (wasActive) prefs.connection = profile;
    await prefs.save();
    if (wasActive) apply(paired.endpoint);
    return profile;
  });

  /// Rename a saved server; its connection is untouched.
  Future<void> rename(String id, String name) => _one(() async {
    final old = server(id);
    if (old == null) return;
    final problem = nameProblem(name, except: id);
    if (problem != null) throw FormatException(problem);
    final next = old.copyWith(name: name.trim());
    _put(next);
    if (current?.id == id) prefs.connection = next;
    await prefs.save();
  });

  /// Forget a saved server and its token. If it's the one in use, the app
  /// goes back to this computer first. This computer can't be removed.
  Future<void> remove(String id) async {
    if (id == LocalProfile.localId) {
      throw const FormatException("This computer can't be removed.");
    }
    final p = server(id);
    if (p == null) return;
    if (current?.id == id) await switchTo(LocalProfile.localId);
    await _one(() async {
      await tokens.delete(p.account);
      prefs.servers.removeWhere((s) => s.id == id);
      prefs.forgetRead(id);
      await prefs.save();
    });
  }

  void _put(RemoteProfile p) {
    final i = prefs.servers.indexWhere((s) => s.id == p.id);
    if (i < 0) {
      prefs.servers.add(p);
    } else {
      prefs.servers[i] = p;
    }
  }
}
