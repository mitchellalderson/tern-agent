import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';

/// Finding and starting `tern daemon` when it isn't running.
///
/// The release build of the macOS app carries its own `tern` in
/// `tern.app/Contents/Helpers`, matched to the app's client, so it comes
/// right after `$TERN_BIN`. Apps started from Finder get a minimal PATH, so
/// common install locations are checked too.
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

/// Where a bundled `tern` would be for the app at [executable]
/// (`<app>.app/Contents/MacOS/<name>`), or null outside an app bundle.
String? bundledTern(String executable) {
  final macos = File(executable).parent;
  if (macos.path.split('/').last != 'MacOS') return null;
  return '${macos.parent.path}/Helpers/tern';
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

  Future<void> setStartAtLogin(bool on) =>
      _tern(['service', if (on) 'install' else 'uninstall']);

  /// Whether the login service is installed and starts tern at login.
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
  AppPrefs(this.file, {this.onClose = OnClose.keepRunning});

  /// `~/Library/Application Support/tern/desktop.json` on macOS,
  /// `$XDG_CONFIG_HOME/tern/desktop.json` elsewhere.
  factory AppPrefs.load([Map<String, String>? env]) {
    env ??= Platform.environment;
    final home = env['HOME'] ?? '';
    final dir = Platform.isMacOS
        ? '$home/Library/Application Support/tern'
        : '${env['XDG_CONFIG_HOME'] ?? '$home/.config'}/tern';
    return AppPrefs.fromFile(File('$dir/desktop.json'));
  }

  factory AppPrefs.fromFile(File file) {
    var onClose = OnClose.keepRunning;
    try {
      final j = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
      if (j['on_close'] == 'quit_tern') onClose = OnClose.quitTern;
    } on Object {
      // Missing or unreadable: defaults.
    }
    return AppPrefs(file, onClose: onClose);
  }

  final File file;
  OnClose onClose;

  Future<void> save() async {
    await file.parent.create(recursive: true);
    await file.writeAsString(
      jsonEncode({
        'on_close': onClose == OnClose.quitTern ? 'quit_tern' : 'keep_running',
      }),
    );
  }
}
