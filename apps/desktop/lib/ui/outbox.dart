import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

/// Writes waiting in the outbox, above Home's composer: what the agent
/// wanted to do while nobody was there to approve it (tooling spec §5.2).
class OutboxStrip extends StatelessWidget {
  const OutboxStrip({super.key, required this.store});

  final TernStore store;

  static const _shown = 3;

  @override
  Widget build(BuildContext context) {
    final pending = store.outboxHere();
    if (pending.isEmpty) return const SizedBox.shrink();
    final c = context.colors;
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 8, 24, 0),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          for (final o in pending.take(_shown))
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: OutboxCard(store: store, item: o),
            ),
          if (pending.length > _shown)
            Text(
              '${pending.length - _shown} more waiting · `tern outbox` lists them all',
              style: TextStyle(fontSize: 12, color: c.muted),
            ),
        ],
      ),
    );
  }
}

class OutboxCard extends StatefulWidget {
  const OutboxCard({super.key, required this.store, required this.item});

  final TernStore store;
  final OutboxItem item;

  @override
  State<OutboxCard> createState() => _OutboxCardState();
}

class _OutboxCardState extends State<OutboxCard> {
  bool _busy = false;

  Future<void> _decide({
    required bool approve,
    Object? input,
    String? note,
  }) async {
    setState(() => _busy = true);
    await widget.store.decideOutbox(
      widget.item,
      approve: approve,
      input: input,
      note: note,
    );
    if (mounted) setState(() => _busy = false);
  }

  Future<void> _edit() async {
    final o = widget.item;
    final text = TextEditingController(
      text: const JsonEncoder.withIndent('  ').convert(o.input),
    );
    String? problem;
    final input = await showDialog<Object>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, setDialog) => AlertDialog(
          title: Text('Edit and approve ${o.tool}'),
          content: SizedBox(
            width: 520,
            child: TextField(
              controller: text,
              maxLines: 14,
              style: mono.copyWith(fontSize: 12.5),
              decoration: InputDecoration(errorText: problem),
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('Cancel'),
            ),
            FilledButton(
              onPressed: () {
                try {
                  final v = jsonDecode(text.text);
                  if (v is! Map) throw const FormatException('not an object');
                  Navigator.pop(context, v);
                } on FormatException {
                  setDialog(() => problem = 'Arguments must be a JSON object');
                }
              },
              child: const Text('Approve with these'),
            ),
          ],
        ),
      ),
    );
    if (input != null) await _decide(approve: true, input: input);
  }

  Future<void> _reject() async {
    final note = TextEditingController();
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Reject'),
        content: SizedBox(
          width: 420,
          child: TextField(
            controller: note,
            autofocus: true,
            decoration: const InputDecoration(
              hintText: 'A note for tern (optional)',
            ),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Reject'),
          ),
        ],
      ),
    );
    if (ok ?? false) await _decide(approve: false, note: note.text);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final o = widget.item;
    final left = o.expiresAt.difference(DateTime.now());
    final expires = left.inHours >= 1
        ? 'expires in ${left.inHours}h'
        : 'expires in ${left.inMinutes.clamp(0, 59)}m';
    final remoteBlocked = o.localOnly && widget.store.remoteCore;
    return Container(
      decoration: BoxDecoration(
        color: c.card,
        border: Border.all(color: c.warn.withValues(alpha: 0.5)),
        borderRadius: BorderRadius.circular(12),
      ),
      padding: const EdgeInsets.all(12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.outbox_outlined, size: 16, color: c.warn),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  o.stale
                      ? 'Out of date · outbox #${o.id}'
                      : 'Waiting for you · outbox #${o.id}',
                  style: const TextStyle(fontWeight: FontWeight.w600),
                ),
              ),
              Text(
                '${relativeTime(o.createdAt)} · $expires',
                style: TextStyle(fontSize: 11.5, color: c.muted),
              ),
            ],
          ),
          const SizedBox(height: 4),
          Text(
            o.stale
                ? o.staleNote
                : 'Nobody was here to approve this, so tern carried on without it.',
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
          if (o.taintNote case final note?) ...[
            const SizedBox(height: 4),
            Row(
              children: [
                Icon(Icons.shield_outlined, size: 13, color: c.warn),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    'Careful: $note',
                    style: TextStyle(fontSize: 12.5, color: c.warn),
                  ),
                ),
              ],
            ),
          ],
          const SizedBox(height: 8),
          Container(
            width: double.infinity,
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              color: c.background,
              borderRadius: BorderRadius.circular(8),
              border: Border.all(color: c.hairline),
            ),
            child: SelectableText(
              o.headline,
              style: mono.copyWith(color: c.text),
            ),
          ),
          const SizedBox(height: 10),
          if (o.stale)
            Row(
              children: [
                FilledButton(
                  onPressed: _busy ? null : () => _decide(approve: false),
                  style: FilledButton.styleFrom(
                    backgroundColor: c.accent,
                    foregroundColor: c.onAccent,
                  ),
                  child: const Text('Discard'),
                ),
              ],
            )
          else if (remoteBlocked)
            Text(
              'Decide this on the computer running tern: it runs a command there.',
              style: TextStyle(fontSize: 12.5, color: c.muted),
            )
          else
            Row(
              children: [
                FilledButton(
                  onPressed: _busy ? null : () => _decide(approve: true),
                  style: FilledButton.styleFrom(
                    backgroundColor: c.accent,
                    foregroundColor: c.onAccent,
                  ),
                  child: const Text('Approve'),
                ),
                const SizedBox(width: 8),
                OutlinedButton(
                  onPressed: _busy ? null : _edit,
                  child: const Text('Edit…'),
                ),
                const SizedBox(width: 8),
                TextButton(
                  onPressed: _busy ? null : _reject,
                  child: Text('Reject', style: TextStyle(color: c.muted)),
                ),
              ],
            ),
        ],
      ),
    );
  }
}
