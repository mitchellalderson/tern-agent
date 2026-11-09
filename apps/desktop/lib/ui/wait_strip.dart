import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

/// A quiet line above the composer for each `wait_until` this conversation
/// is waiting on: "Waiting: `<note>`" and a Cancel link. The daemon polls the
/// check and continues the conversation by itself; nothing here needs doing.
class WaitStrip extends StatefulWidget {
  const WaitStrip({
    super.key,
    required this.store,
    this.refreshEvery = const Duration(seconds: 5),
  });

  final TernStore store;
  final Duration refreshEvery;

  @override
  State<WaitStrip> createState() => _WaitStripState();
}

class _WaitStripState extends State<WaitStrip> {
  List<Wait> _waits = const [];
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _load();
    _timer = Timer.periodic(widget.refreshEvery, (_) => _load());
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    if (!widget.store.hasWaits) return;
    try {
      final waits = await widget.store.client.waits();
      if (mounted) setState(() => _waits = waits);
    } on TernException {
      // An older core has no waits; show nothing.
      if (mounted && _waits.isNotEmpty) setState(() => _waits = const []);
    }
  }

  Future<void> _cancel(Wait w) async {
    try {
      await widget.store.client.cancelWait(w.id);
    } on TernException {
      // Already ended; the next load drops it.
    }
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final store = widget.store;
    final here = [
      for (final w in _waits)
        if (store.streamOpen
            ? w.channelId == store.streamChannel
            : w.sessionId == store.selected)
          w,
    ];
    if (!store.hasWaits || here.isEmpty) return const SizedBox.shrink();
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 8, 24, 0),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          for (final w in here)
            Row(
              children: [
                Icon(Icons.hourglass_top, size: 13, color: c.muted),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    'Waiting: ${w.note}',
                    style: TextStyle(fontSize: 12, color: c.muted),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
                TextButton(
                  onPressed: () => _cancel(w),
                  style: TextButton.styleFrom(
                    foregroundColor: c.muted,
                    visualDensity: VisualDensity.compact,
                    textStyle: const TextStyle(fontSize: 12),
                  ),
                  child: const Text('Cancel'),
                ),
              ],
            ),
        ],
      ),
    );
  }
}
