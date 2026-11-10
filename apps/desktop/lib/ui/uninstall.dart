import 'dart:io';
import 'dart:ui' show AppExitType;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../state/daemon.dart';
import '../theme.dart';

/// Where the app lives once installed from the dmg.
const appBundlePath = '/Applications/tern.app';

/// Settings › About › Uninstall tern…: stop tern and remove its login items
/// with `tern uninstall`, optionally deleting the data too. Only for the
/// tern on this computer; the caller checks that.
Future<void> showUninstallDialog(
  BuildContext context, {
  required TernControl control,
  required String dataDir,
  required Future<void> Function(String path) reveal,
  Future<void> Function()? exitApp,
  bool macOS = false,
}) => showDialog<void>(
  context: context,
  barrierDismissible: false,
  builder: (_) => UninstallDialog(
    control: control,
    dataDir: dataDir,
    reveal: reveal,
    exitApp: exitApp,
    macOS: macOS || Platform.isMacOS,
  ),
);

class UninstallDialog extends StatefulWidget {
  const UninstallDialog({
    super.key,
    required this.control,
    required this.dataDir,
    required this.reveal,
    this.exitApp,
    this.macOS = false,
  });

  final TernControl control;
  final String dataDir;
  final Future<void> Function(String path) reveal;
  final Future<void> Function()? exitApp;
  final bool macOS;

  @override
  State<UninstallDialog> createState() => _UninstallDialogState();
}

class _UninstallDialogState extends State<UninstallDialog> {
  var _purge = false;
  var _running = false;
  var _done = false;
  String? _error;

  Future<void> _quit() async {
    final exit =
        widget.exitApp ??
        () async {
          await ServicesBinding.instance.exitApplication(AppExitType.required);
        };
    await exit();
  }

  Future<void> _uninstall() async {
    setState(() {
      _running = true;
      _error = null;
    });
    try {
      await widget.control.uninstall(purge: _purge);
      if (mounted) setState(() => _done = true);
    } on TernCommandException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _running = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final muted = TextStyle(fontSize: 13, color: c.muted);
    if (_done) {
      return AlertDialog(
        title: const Text('tern is removed'),
        content: Text(
          'tern is stopped and removed from login items. '
                  '${widget.macOS ? 'Drag tern to the Trash to finish.' : ''}'
              .trim(),
        ),
        actions: [
          TextButton(
            key: const Key('uninstall-quit'),
            onPressed: _quit,
            child: const Text('Quit'),
          ),
          if (widget.macOS)
            FilledButton(
              key: const Key('uninstall-finder'),
              onPressed: () async {
                await widget.reveal(appBundlePath);
                await _quit();
              },
              child: const Text('Show in Finder'),
            ),
        ],
      );
    }
    return AlertDialog(
      title: const Text('Uninstall tern?'),
      content: SizedBox(
        width: 380,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Stops tern and removes it from login items.', style: muted),
            const SizedBox(height: 8),
            CheckboxListTile(
              key: const Key('uninstall-purge'),
              contentPadding: EdgeInsets.zero,
              controlAffinity: ListTileControlAffinity.leading,
              title: const Text('Also delete my data'),
              value: _purge,
              onChanged: _running ? null : (v) => setState(() => _purge = v!),
            ),
            if (_purge) ...[
              SelectableText(
                widget.dataDir,
                key: const Key('uninstall-path'),
                style: mono.copyWith(fontSize: 12.5),
              ),
              const SizedBox(height: 6),
              Text(
                'Includes API keys, agents and memory. This can’t be undone.',
                key: const Key('uninstall-warning'),
                style: TextStyle(fontSize: 12.5, color: c.error),
              ),
            ],
            if (_error != null) ...[
              const SizedBox(height: 10),
              Text(
                _error!,
                key: const Key('uninstall-error'),
                style: TextStyle(fontSize: 12.5, color: c.error),
              ),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: _running ? null : () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('uninstall-confirm'),
          onPressed: _running ? null : _uninstall,
          child: Text(_running ? 'Uninstalling…' : 'Uninstall'),
        ),
      ],
    );
  }
}
