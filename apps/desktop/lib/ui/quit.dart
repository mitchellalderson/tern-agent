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
