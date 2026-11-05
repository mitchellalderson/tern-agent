import 'dart:ui' show AppExitType;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:tern_client/tern_client.dart';

import '../state/daemon.dart';
import '../state/store.dart';

/// The confirmation's body (spec §11.5a): what stopping interrupts, and that
/// it will pick up again.
String quitMessage(List<PausedRun> runs) {
  final names = runs.map((r) => '"${r.title}"').join(', ');
  final (count, pronoun) = runs.length == 1
      ? ('1 task is', 'It')
      : ('${runs.length} tasks are', 'They');
  return '$count in progress ($names). $pronoun will pick up where '
      '${runs.length == 1 ? 'it' : 'they'} left off next time tern starts.\n'
      'Routines and Telegram will be paused until then.';
}

/// Asks only when something would be interrupted. True to go ahead.
Future<bool> confirmQuit(BuildContext context, List<PausedRun> runs) async {
  if (runs.isEmpty) return true;
  final ok = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: const Text('Quit tern?'),
      content: Text(quitMessage(runs)),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context, false),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(context, true),
          child: const Text('Quit tern'),
        ),
      ],
    ),
  );
  return ok ?? false;
}

/// What the user picked when closing the window would leave tern running
/// with no menu-bar icon.
enum CloseChoice { keepRunning, stop, cancel }

/// True when nothing would show that tern keeps running after the window
/// closes: it isn't started at login (so there is no tray icon either), or
/// that can't be told.
Future<bool> daemonHasNoTray(TernControl control) async =>
    !((await control.loginState())?.atLogin ?? false);

/// One question on closing the window while tern would stay running with no
/// menu-bar icon: it holds API access and runs routines, so say so and let
/// the user stop it here.
Future<CloseChoice> askCloseTrayless(BuildContext context) async {
  final choice = await showDialog<CloseChoice>(
    context: context,
    builder: (context) => AlertDialog(
      title: const Text('Leave tern running?'),
      content: const Text(
        'tern keeps running in the background after you close the window, so '
        'routines and Telegram keep working. It has no menu-bar icon '
        'because it is not set to start when you log in, so the only ways to '
        'stop it later are this app or `tern stop`.\n\n'
        'Settings › General › Start tern when I log in adds the icon.',
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context, CloseChoice.cancel),
          child: const Text('Cancel'),
        ),
        TextButton(
          onPressed: () => Navigator.pop(context, CloseChoice.keepRunning),
          child: const Text('Keep running'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(context, CloseChoice.stop),
          child: const Text('Stop tern'),
        ),
      ],
    ),
  );
  return choice ?? CloseChoice.cancel;
}

/// tern › Quit tern completely (⌥⌘Q): confirm if a task is in progress,
/// stop tern, then quit the app. Returns false when the user cancelled.
/// [exitApp] defaults to quitting this process.
Future<bool> quitTernCompletely(
  BuildContext context,
  TernStore store,
  TernControl control, {
  Future<void> Function()? exitApp,
  Duration wait = const Duration(seconds: 10),
}) async {
  exitApp ??= () async {
    await ServicesBinding.instance.exitApplication(AppExitType.required);
  };
  // A remote core isn't ours to stop; only the window closes.
  if (store.health?.isLocal ?? false) {
    List<PausedRun> runs;
    try {
      runs = (await store.client.shutdown(dryRun: true)).runs;
    } on TernException {
      runs = const [];
    }
    if (!context.mounted || !await confirmQuit(context, runs)) return false;
    if (!context.mounted) return false;
    showDialog<void>(
      context: context,
      barrierDismissible: false,
      builder: (_) => const AlertDialog(
        content: Row(
          children: [
            SizedBox.square(
              dimension: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
            SizedBox(width: 14),
            Text('Quitting tern…'),
          ],
        ),
      ),
    );
    try {
      await stopTern(store.client, control, wait: wait);
    } on TernCommandException catch (e) {
      if (context.mounted) {
        Navigator.pop(context);
        await showDialog<void>(
          context: context,
          builder: (context) => AlertDialog(
            title: const Text("tern didn't stop"),
            content: Text(e.message),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context),
                child: const Text('OK'),
              ),
            ],
          ),
        );
      }
      return false;
    }
    if (context.mounted) Navigator.pop(context);
  }
  await exitApp();
  return true;
}
