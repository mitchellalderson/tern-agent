import 'package:flutter/material.dart';

import '../state/daemon.dart';
import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

/// Shown until the app has reached the daemon once: a calm setup screen that
/// starts tern on this Mac.
class OfflineView extends StatefulWidget {
  const OfflineView({
    super.key,
    required this.store,
    this.locateBin = findTern,
  });

  final TernStore store;

  /// Finds the `tern` binary; replaceable in tests.
  final String? Function() locateBin;

  @override
  State<OfflineView> createState() => _OfflineViewState();
}

class _OfflineViewState extends State<OfflineView> {
  late final String? _bin = widget.locateBin();
  String? _error;
  var _starting = false;
  var _details = false;

  Future<void> _start() async {
    setState(() {
      _starting = true;
      _error = null;
    });
    try {
      await startDaemon(_bin!);
      await Future<void>.delayed(const Duration(milliseconds: 600));
      await widget.store.connect();
    } on Exception catch (e) {
      _error = e.toString();
    }
    if (mounted) setState(() => _starting = false);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final store = widget.store;
    final connecting = store.link == Link.connecting;
    final failed = _error != null;
    final detail = _error ?? store.linkError;
    final title = connecting ? 'Connecting…' : 'Welcome to tern';
    final body = connecting
        ? null
        : failed
        ? "tern didn't start. Try again."
        : 'Start tern on this Mac to get going.';
    return WindowDragArea(
      child: Container(
        color: c.background,
        alignment: Alignment.center,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 380),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const AgentAvatar(name: 'tern', size: 52),
              const SizedBox(height: 20),
              Text(
                title,
                style: const TextStyle(
                  fontSize: 20,
                  fontWeight: FontWeight.w600,
                ),
              ),
              if (body != null) ...[
                const SizedBox(height: 8),
                Text(
                  body,
                  textAlign: TextAlign.center,
                  style: TextStyle(fontSize: 13, color: c.muted),
                ),
              ],
              if (store.link == Link.offline) ...[
                const SizedBox(height: 28),
                if (_bin != null)
                  FilledButton(
                    onPressed: _starting ? null : _start,
                    style: FilledButton.styleFrom(
                      backgroundColor: c.accent,
                      foregroundColor: c.onAccent,
                      minimumSize: const Size(220, 44),
                    ),
                    child: Text(_starting ? 'Starting…' : 'Start on this Mac'),
                  )
                else
                  SelectableText(
                    "Couldn't find tern on this Mac. Start it yourself:\n"
                    'tern start',
                    textAlign: TextAlign.center,
                    style: mono.copyWith(color: c.muted),
                  ),
                if (failed) ...[
                  const SizedBox(height: 4),
                  TextButton(
                    onPressed: _starting ? null : _start,
                    style: TextButton.styleFrom(foregroundColor: c.muted),
                    child: const Text('Try again'),
                  ),
                ],
                const SizedBox(height: 12),
                TextButton(
                  key: const Key('offline-details-toggle'),
                  onPressed: () => setState(() => _details = !_details),
                  style: TextButton.styleFrom(
                    foregroundColor: c.muted,
                    textStyle: const TextStyle(fontSize: 12),
                  ),
                  child: Text(_details ? 'Hide details' : 'Details'),
                ),
                if (_details || failed)
                  Padding(
                    padding: const EdgeInsets.only(top: 4),
                    child: SelectableText(
                      [
                        store.client.endpoint.describe(),
                        ?detail,
                      ].join('\n'),
                      key: const Key('offline-details'),
                      textAlign: TextAlign.center,
                      style: TextStyle(fontSize: 12, color: c.muted),
                    ),
                  ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}
