import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';

import 'connection.dart';

/// Finding and starting `tern daemon` when it isn't running.
///
/// The release builds carry their own `tern`, matched to the app's client:
/// on Linux beside the app's executable, on macOS inside the menu-bar helper
/// (`tern.app/Contents/Library/LoginItems/tern Tray.app`), so it comes right
/// after `$TERN_BIN`. Apps started from Finder get a minimal PATH, so common
/// install locations are checked too.
String? findTern([Map<String, String>? env, String? executable]) {
  env ??= Platform.environment;
  final home = env['HOME'] ?? '';
  final candidates = [
    ?env['TERN_BIN'],
    ?bundledTern(executable ?? Platform.resolvedExecutable),
    for (final dir in (env['PATH'] ?? '').split(':'))
      if (dir.isNotEmpty) '$dir/tern',
    '$home/.cargo/bin/tern',
    '$home/.local/bin/tern',
    '/opt/homebrew/bin/tern',
    '/usr/local/bin/tern',
  ];
  for (final c in candidates) {
    if (File(c).existsSync()) return c;
  }
  return null;
}

/// Where a bundled `tern` would be for the app at [executable], or null
/// when there's nothing to look for.
///
/// macOS (`<app>.app/Contents/MacOS/<name>`): the tray helper's executable.
/// Run with arguments it's the ordinary CLI, and `service install` from
/// there registers login items with `SMAppService`.
///
/// Linux (`scripts/package-linux-app.sh`): a `tern` beside the app's own
/// executable, which is also where the tray's "Open tern" looks for
/// `tern_desktop`. [findTern] checks that it exists.
String? bundledTern(String executable) {
  final dir = File(executable).parent;
  if (dir.path.split('/').last != 'MacOS') {
    return Platform.isLinux ? '${dir.path}/tern' : null;
  }
  return '${dir.parent.path}/Library/LoginItems/tern Tray.app/Contents/MacOS/tern-tray';
}

/// Start tern with `tern start`: through the login service when it's
/// installed, otherwise detached, so it outlives the app.
Future<void> startDaemon(String bin) => TernControl(bin: bin).start();

/// Runs `tern <args>`; replaced in tests.
typedef RunTern = Future<ProcessResult> Function(String bin, List<String> args);

Future<ProcessResult> _runTern(String bin, List<String> args) =>
    Process.run(bin, args);

/// A `tern` command that failed, with what it printed.
class TernCommandException implements Exception {
  const TernCommandException(this.message);

  final String message;

  @override
  String toString() => message;
}

/// Starting and stopping tern and its login service with the `tern`
/// binary (personal-agent-tooling-spec §11.5a), so nobody needs a terminal.
class TernControl {
  TernControl({this.bin, Map<String, String>? env, this.run = _runTern})
    : env = env ?? Platform.environment;

  final String? bin;
  final Map<String, String> env;
  final RunTern run;

  Future<void> start() => _tern(['start']);

  /// `tern stop`: asks tern to stop, then stops it by force if it doesn't
  /// within 10 s, so the service manager doesn't bring it back.
  Future<void> stop() => _tern(['stop']);

  /// `tern uninstall`: stop tern and remove its login items and menu-bar
  /// icon. With [purge], also delete the data folder (`--yes`: the app has
  /// already asked).
  Future<void> uninstall({bool purge = false}) => _tern([
    'uninstall',
    if (purge) ...['--purge', '--yes'],
  ]);

  /// Start tern (and, on macOS, the menu-bar icon) at login, or stop
  /// doing so. Either way tern keeps running.
  Future<void> setStartAtLogin(bool on) => _tern([
    'service',
    if (on) ...['install', if (Platform.isMacOS) '--tray'] else 'uninstall',
  ]);

  /// What `tern service status --json` says, or null if it can't be asked
  /// (no tern, or one too old to answer).
  Future<LoginState?> loginState() async {
    final b = bin;
    if (b == null) return null;
    try {
      final r = await run(b, ['service', 'status', '--json']);
      if (r.exitCode != 0) return null;
      return LoginState.fromJson(
        jsonDecode('${r.stdout}'.trim()) as Map<String, dynamic>,
      );
    } on Object {
      return null;
    }
  }

  /// Whether a plain login service file starts tern at login; the quick
  /// guess until [loginState] answers.
  bool get startsAtLogin {
    final home = env['HOME'] ?? '';
    if (Platform.isMacOS) {
      final plist = File('$home/Library/LaunchAgents/ai.tern.daemon.plist');
      return plist.existsSync() &&
          plist.readAsStringSync().contains('<key>RunAtLoad</key>\n\t<true/>');
    }
    final config = env['XDG_CONFIG_HOME'] ?? '$home/.config';
    return File('$config/systemd/user/default.target.wants/tern-daemon.service')
        .existsSync();
  }

  Future<void> _tern(List<String> args) async {
    final b = bin;
    if (b == null) {
      throw const TernCommandException("Couldn't find tern on this computer.");
    }
    final r = await run(b, args);
    if (r.exitCode != 0) {
      final out = '${r.stderr}'.trim();
      throw TernCommandException(
        out.isEmpty ? 'tern ${args.join(' ')} failed' : out,
      );
    }
  }
}

/// How tern starts at login, from `tern service status --json`.
class LoginState {
  const LoginState({required this.atLogin, this.needsApproval = false});

  factory LoginState.fromJson(Map<String, dynamic> j) => LoginState(
    atLogin: j['at_login'] == true,
    needsApproval: j['needs_approval'] == true,
  );

  final bool atLogin;

  /// Registered, but macOS waits for the user to allow it in System
  /// Settings › General › Login Items.
  final bool needsApproval;
}

/// Stop tern: ask it to stop (it exits cleanly, so the login service
/// doesn't restart it), wait up to [wait] for it to go, then fall back to
/// `tern stop`, which stops it by force. Runs in progress resume on the
/// next start.
Future<void> stopTern(
  TernClient client,
  TernControl control, {
  Duration wait = const Duration(seconds: 10),
}) async {
  try {
    await client.shutdown();
  } on TernException catch (e) {
    if (e.unreachable) return;
  }
  final end = DateTime.now().add(wait);
  while (DateTime.now().isBefore(end)) {
    try {
      await client.health();
    } on TernException {
      return;
    }
    await Future<void>.delayed(const Duration(milliseconds: 200));
  }
  await control.stop();
}

/// What closing the window does.
enum OnClose { keepRunning, quitTern }

/// The app's own settings, in a small JSON file outside tern's data
/// directory.
class AppPrefs {
  AppPrefs(
    this.file, {
    this.onClose = OnClose.keepRunning,
    this.connection,
    List<RemoteProfile>? servers,
    Map<String, int>? read,
  }) : servers = servers ?? [] {
    if (read != null) _read[connection?.id ?? LocalProfile.localId] = read;
  }

  /// `~/Library/Application Support/tern/desktop.json` on macOS,
  /// `$XDG_CONFIG_HOME/tern/desktop.json` elsewhere.
  factory AppPrefs.load([Map<String, String>? env]) =>
      AppPrefs.fromFile(File('${dir(env)}/desktop.json'));

  /// The app's own settings directory.
  static String dir([Map<String, String>? env]) {
    env ??= Platform.environment;
    final home = env['HOME'] ?? '';
    return Platform.isMacOS
        ? '$home/Library/Application Support/tern'
        : '${env['XDG_CONFIG_HOME'] ?? '$home/.config'}/tern';
  }

  factory AppPrefs.fromFile(File file) {
    var onClose = OnClose.keepRunning;
    Profile? connection;
    final servers = <RemoteProfile>[];
    final byServer = <String, Map<String, int>>{};
    Map<String, int> ints(Object? m) => {
      if (m is Map)
        for (final e in m.entries)
          if (e.value case final int v) '${e.key}': v,
    };
    try {
      final j = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
      if (j['on_close'] == 'quit_tern') onClose = OnClose.quitTern;
      connection = Profile.fromJson(j['connection']);
      for (final s in j['servers'] as List? ?? const []) {
        if (RemoteProfile.fromJson(s as Map<String, dynamic>) case final p?
            when !servers.any((x) => x.id == p.id)) {
          servers.add(p);
        }
      }
      // An app that kept one server saved just `connection`: it becomes the
      // first of the list, with its token where it was.
      if (connection case final RemoteProfile c) {
        final i = servers.indexWhere((s) => s.id == c.id);
        if (i < 0) {
          servers.add(c);
        } else {
          connection = servers[i];
        }
      }
      if (j['read_by_server'] case final Map m) {
        for (final e in m.entries) {
          byServer['${e.key}'] = ints(e.value);
        }
      } else if (j['read'] case final Map m) {
        // Before several servers: what was read belongs to the one in use.
        byServer[connection?.id ?? LocalProfile.localId] = ints(m);
      }
    } on Object {
      // Missing or unreadable: defaults.
    }
    return AppPrefs(file, onClose: onClose, connection: connection)
      ..servers.addAll(servers)
      .._read.addAll(byServer);
  }

  final File file;
  OnClose onClose;

  /// Where tern runs; null until the first-launch chooser is answered.
  Profile? connection;

  /// The saved servers (this computer is always there and isn't listed).
  /// Their tokens are in the Keychain, never here.
  final List<RemoteProfile> servers;

  final Map<String, Map<String, int>> _read = {};

  /// Channel → the newest post seen here, for the server in use (unread
  /// dots; not synced). Channel ids repeat across servers, so each one
  /// keeps its own.
  Map<String, int> get read =>
      _read.putIfAbsent(connection?.id ?? LocalProfile.localId, () => {});

  /// Drop what was read on a server that was removed.
  void forgetRead(String serverId) => _read.remove(serverId);

  Future<void> save() async {
    await file.parent.create(recursive: true);
    await file.writeAsString(
      jsonEncode({
        'on_close': onClose == OnClose.quitTern ? 'quit_tern' : 'keep_running',
        // Still written, so an older app finds the server in use.
        'connection': ?connection?.toJson(),
        if (servers.isNotEmpty)
          'servers': [for (final s in servers) s.toJson()],
        if (_read.values.any((m) => m.isNotEmpty))
          'read_by_server': {
            for (final e in _read.entries)
              if (e.value.isNotEmpty) e.key: e.value,
          },
      }),
    );
  }
}
