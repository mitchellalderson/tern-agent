import 'dart:io';

import 'package:flutter/material.dart';

import '../state/daemon.dart';
import '../theme.dart';

/// On first launch, offer what Settings › General's "Start tern when I log
/// in" does, so nobody has to find it (personal-agent-tooling-spec §11.5,
/// T8b). Asked once: either answer, or dismissing it, counts.
Future<void> maybeAskKeepRunning(
  BuildContext context,
  TernControl control,
  AppPrefs prefs,
) async {
  if (prefs.askedBackground || control.bin == null) return;
  final state = await control.loginState();
  if (state?.atLogin ?? control.startsAtLogin) {
    await _remember(prefs);
    return;
  }
  if (!context.mounted) return;
  await showDialog<void>(
    context: context,
    builder: (_) => KeepRunningDialog(control: control),
  );
  await _remember(prefs);
}

Future<void> _remember(AppPrefs prefs) async {
  prefs.askedBackground = true;
  try {
    await prefs.save();
  } on Exception {
    // Asked again next launch; nothing else depends on it.
  }
}

class KeepRunningDialog extends StatefulWidget {
  const KeepRunningDialog({super.key, required this.control});

  final TernControl control;

  @override
  State<KeepRunningDialog> createState() => _KeepRunningDialogState();
}

class _KeepRunningDialogState extends State<KeepRunningDialog> {
  var _busy = false;
  String? _error;

  Future<void> _startAtLogin() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.control.setStartAtLogin(true);
      if (mounted) Navigator.pop(context);
    } on TernCommandException catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = e.message;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return AlertDialog(
      title: const Text('Keep tern running in the background?'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 420),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              Platform.isMacOS
                  ? 'tern can start when you log in and sit in the menu bar, '
                        'so routines and Telegram keep working when this '
                        'window is closed. Quit it any time from the menu-bar '
                        'icon.'
                  : 'tern can start when you log in, so routines and '
                        'Telegram keep working when this window is closed.',
            ),
            const SizedBox(height: 8),
            Text(
              'You can change this later in Settings.',
              style: TextStyle(fontSize: 12, color: c.muted),
            ),
            if (_error case final e?) ...[
              const SizedBox(height: 8),
              Text(e, style: TextStyle(fontSize: 12, color: c.error)),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.pop(context),
          child: const Text('Not now'),
        ),
        FilledButton(
          onPressed: _busy ? null : _startAtLogin,
          child: Text(_busy ? 'Setting up…' : 'Start at login'),
        ),
      ],
    );
  }
}
