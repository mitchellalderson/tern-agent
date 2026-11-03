import 'dart:io';

/// Finding and starting `tern daemon` when it isn't running.
///
/// Apps started from Finder get a minimal PATH, so common install
/// locations are checked too.
String? findTern([Map<String, String>? env]) {
  env ??= Platform.environment;
  final home = env['HOME'] ?? '';
  final candidates = [
    ?env['TERN_BIN'],
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

/// Start the daemon detached, so it outlives the app.
Future<void> startDaemon(String bin) async {
  await Process.start(bin, ['daemon'], mode: ProcessStartMode.detached);
}
