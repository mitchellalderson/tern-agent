import 'dart:io';

import 'package:flutter/material.dart';

import '../state/connection.dart';
import '../state/daemon.dart';
import '../state/store.dart';
import '../theme.dart';
import 'servers.dart';

/// Settings › General: where tern runs, and (on this computer) starting
/// it at login and what closing the window does (spec §11.5a). On a
/// server those don't apply, so the server's name and status show
/// instead (§11.5, "Remote cores").
class SettingsDialog extends StatefulWidget {
  const SettingsDialog({
    super.key,
    required this.control,
    required this.prefs,
    this.store,
    this.connections,
    this.embedded = false,
    this.onOpenServers,
  });

  final TernControl control;
  final AppPrefs prefs;
  final TernStore? store;
  final Connections? connections;

  /// Shown inside the Settings window's General section, which supplies the
  /// window, the title and Done, and shows the security audit under
  /// Permissions.
  final bool embedded;

  /// Embedded: switch to the Servers section instead of opening its dialog.
  final VoidCallback? onOpenServers;

  @override
  State<SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends State<SettingsDialog> {
  late bool _atLogin = widget.control.startsAtLogin;
  var _needsApproval = false;
  var _busy = false;
  String? _error;

  RemoteProfile? get _server => switch (widget.connections?.current) {
    RemoteProfile p => p,
    _ => null,
  };

  @override
  void initState() {
    super.initState();
    if (_server == null) _refresh();
  }

  Future<void> _changeServer() async {
    final c = widget.connections;
    if (c == null) return;
    if (widget.onOpenServers case final open?) return open();
    final before = c.activeId;
    await showServersDialog(context, c);
    // The app now shows another server: this dialog belonged to the old.
    if (mounted && c.activeId != before) Navigator.pop(context);
  }

  Future<void> _useThisComputer() async {
    setState(() => _busy = true);
    try {
      await widget.connections?.useLocal();
      if (mounted) Navigator.pop(context);
    } on Exception catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = '$e';
        });
      }
    }
  }

  Widget _where(BuildContext context) {
    final small = Theme.of(context).textTheme.bodySmall;
    final server = _server;
    final store = widget.store;
    final status = store == null
        ? null
        : switch (store.link) {
            Link.online =>
              'Connected · tern ${store.health?.version ?? ''}'.trim(),
            Link.connecting => 'Connecting…',
            Link.offline => store.linkError ?? "Can't reach it right now",
          };
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ListTile(
          contentPadding: EdgeInsets.zero,
          title: Text(
            server == null
                ? 'tern runs on this ${Platform.isMacOS ? 'Mac' : 'computer'}'
                : 'tern runs on ${server.name}',
          ),
          subtitle: Text(
            [
              if (server != null) server.url.toString(),
              if (server != null && status != null) status,
            ].join('\n'),
            style: small,
          ),
        ),
        Wrap(
          spacing: 8,
          children: [
            OutlinedButton(
              onPressed: _busy ? null : _changeServer,
              child: const Text('Servers…'),
            ),
            if (server != null)
              TextButton(
                onPressed: _busy ? null : _useThisComputer,
                child: Text(
                  'Run on this ${Platform.isMacOS ? 'Mac' : 'computer'} '
                  'instead',
                ),
              ),
          ],
        ),
        const SizedBox(height: 12),
      ],
    );
  }

  /// Ask tern, which also knows about login items registered through
  /// macOS (and switched off in System Settings).
  Future<void> _refresh() async {
    final s = await widget.control.loginState();
    if (s == null || !mounted) return;
    setState(() {
      _atLogin = s.atLogin || s.needsApproval;
      _needsApproval = s.needsApproval;
    });
  }

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
    await _refresh();
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
    final embedded = widget.embedded;
    final content = Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (!embedded) ...[
          const Text(
            'Settings',
            style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
          ),
          const SizedBox(height: 14),
        ],
        Text(
          'This app',
          style: TextStyle(
            fontSize: 12,
            fontWeight: FontWeight.w600,
            color: c.muted,
          ),
        ),
        if (widget.connections != null) _where(context),
        if (_server == null) ...[
          SwitchListTile(
            contentPadding: EdgeInsets.zero,
            title: const Text('Start tern when I log in'),
            subtitle: Text(
              !canInstall
                  ? "Couldn't find tern on this computer."
                  : _needsApproval
                  ? 'macOS needs your OK: turn tern on in System '
                        'Settings › General › Login Items.'
                  : Platform.isMacOS
                  ? 'tern runs in the background with an icon in the '
                        'menu bar, so routines and Telegram keep working '
                        'with this window closed.'
                  : 'tern runs in the background, so routines and '
                        'Telegram keep working with this window closed.',
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
        ],
        if (_error case final e?) ...[
          const SizedBox(height: 8),
          Text(e, style: TextStyle(fontSize: 12, color: c.error)),
        ],
        if (!embedded) ...[
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerRight,
            child: TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('Done'),
            ),
          ),
        ],
      ],
    );
    if (embedded) return content;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: content,
        ),
      ),
    );
  }
}
