import 'package:flutter/material.dart';

import '../state/daemon.dart';
import '../theme.dart';

Future<void> showSettingsDialog(
  BuildContext context,
  TernControl control,
  AppPrefs prefs,
) => showDialog<void>(
  context: context,
  builder: (_) => SettingsDialog(control: control, prefs: prefs),
);

/// Settings › General: starting tern at login and what closing the window
/// does (spec §11.5a).
class SettingsDialog extends StatefulWidget {
  const SettingsDialog({super.key, required this.control, required this.prefs});

  final TernControl control;
  final AppPrefs prefs;

  @override
  State<SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends State<SettingsDialog> {
  late bool _atLogin = widget.control.startsAtLogin;
  var _busy = false;
  String? _error;

  Future<void> _setAtLogin(bool on) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.control.setStartAtLogin(on);
      _atLogin = on;
    } on TernCommandException catch (e) {
      _error = e.message;
    }
    if (mounted) setState(() => _busy = false);
  }

  Future<void> _setOnClose(OnClose? v) async {
    if (v == null) return;
    setState(() => widget.prefs.onClose = v);
    try {
      await widget.prefs.save();
    } on Exception catch (e) {
      if (mounted) setState(() => _error = "Couldn't save: $e");
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final canInstall = widget.control.bin != null;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text(
                'Settings',
                style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 14),
              Text(
                'General',
                style: TextStyle(
                  fontSize: 12,
                  fontWeight: FontWeight.w600,
                  color: c.muted,
                ),
              ),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: const Text('Start tern when I log in'),
                subtitle: Text(
                  canInstall
                      ? 'tern runs in the background, so routines and '
                            'Telegram keep working with this window closed.'
                      : "Couldn't find tern on this computer.",
                  style: small,
                ),
                value: _atLogin,
                onChanged: _busy || !canInstall ? null : _setAtLogin,
              ),
              const SizedBox(height: 10),
              const Text('When I close the window:'),
              RadioGroup<OnClose>(
                groupValue: widget.prefs.onClose,
                onChanged: _setOnClose,
                child: const Column(
                  children: [
                    RadioListTile<OnClose>(
                      contentPadding: EdgeInsets.zero,
                      value: OnClose.keepRunning,
                      title: Text(
                        'Keep tern running in the background (recommended)',
                      ),
                    ),
                    RadioListTile<OnClose>(
                      contentPadding: EdgeInsets.zero,
                      value: OnClose.quitTern,
                      title: Text('Quit tern completely'),
                    ),
                  ],
                ),
              ),
              if (_error case final e?) ...[
                const SizedBox(height: 8),
                Text(e, style: TextStyle(fontSize: 12, color: c.error)),
              ],
              const SizedBox(height: 8),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: () => Navigator.pop(context),
                  child: const Text('Done'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
