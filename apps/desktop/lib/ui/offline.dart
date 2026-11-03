import 'package:flutter/material.dart';

import '../state/daemon.dart';
import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

/// Shown until the app has reached the daemon once.
class OfflineView extends StatefulWidget {
  const OfflineView({super.key, required this.store});

  final TernStore store;

  @override
  State<OfflineView> createState() => _OfflineViewState();
}

class _OfflineViewState extends State<OfflineView> {
  final _bin = findTern();
  String? _error;
  var _starting = false;

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
    return WindowDragArea(
      child: Container(
        color: c.background,
        alignment: Alignment.center,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const AgentAvatar(name: 'tern', size: 52),
              const SizedBox(height: 18),
              Text(
                store.link == Link.connecting
                    ? 'Connecting to tern…'
                    : "tern isn't running",
                style: const TextStyle(
                  fontSize: 18,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 8),
              Text(
                'The app talks to the tern daemon on\n'
                '${store.client.endpoint.describe()}',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 20),
              if (store.link == Link.offline) ...[
                if (_bin != null)
                  FilledButton(
                    onPressed: _starting ? null : _start,
                    style: FilledButton.styleFrom(
                      backgroundColor: c.accent,
                      foregroundColor: c.onAccent,
                    ),
                    child: Text(_starting ? 'Starting…' : 'Start tern'),
                  )
                else
                  SelectableText(
                    "Couldn't find the tern binary. Start it yourself:\n"
                    'tern daemon',
                    textAlign: TextAlign.center,
                    style: mono.copyWith(color: c.muted),
                  ),
                const SizedBox(height: 8),
                TextButton(
                  onPressed: store.connect,
                  child: const Text('Try again'),
                ),
              ],
              if (_error ?? store.linkError case final e?) ...[
                const SizedBox(height: 12),
                Text(
                  e,
                  textAlign: TextAlign.center,
                  style: TextStyle(fontSize: 12, color: c.muted),
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}
