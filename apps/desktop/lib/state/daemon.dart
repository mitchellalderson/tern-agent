import 'dart:io';

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

/// Start the daemon detached, so it outlives the app.
Future<void> startDaemon(String bin) async {
  await Process.start(bin, ['daemon'], mode: ProcessStartMode.detached);
}
