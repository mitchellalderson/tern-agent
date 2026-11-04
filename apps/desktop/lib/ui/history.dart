import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'channel_items.dart';
import 'move_dialog.dart';
import 'widgets.dart';

/// Home's History (docs/channels-design.md §5.2): earlier sessions of the
/// stream, Telegram chats, and conversations from before channels (old
/// side chats among them, as plain past conversations). Each opens in the
/// right pane to read; the stream itself is where conversation goes on.
Future<void> showHistory(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => HistoryDialog(store: store),
    );

class HistoryDialog extends StatefulWidget {
  const HistoryDialog({super.key, required this.store});

  final TernStore store;

  @override
  State<HistoryDialog> createState() => _HistoryDialogState();
}

class _HistoryDialogState extends State<HistoryDialog> {
  final _search = TextEditingController();
  List<SessionItem>? _all;
  String? _error;

  /// Picked to move together into a channel.
  final _picked = <String>{};

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final all = await widget.store.client.sessions(
        limit: 200,
        channel: widget.store.streamChannel,
      );
      if (mounted) setState(() => _all = all);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  /// What kind of entry it is, in words.
  static String kind(SessionItem s, String? head) {
    if (s.id == head) return 'Current session';
    // Side chats were removed; old ones are earlier conversations.
    if (s.branchedFrom != null) return 'Earlier conversation';
    if (s.source == 'telegram') return 'Telegram';
    if (s.segmentReason == 'migrated') return 'Earlier conversation';
    if (s.segmentReason case final r?) {
      return 'Session · ${CutDivider.reason(r)}';
    }
    return 'Session';
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final q = _search.text.trim().toLowerCase();
    final head = widget.store.stream?.mainSessionId;
    final shown = (_all ?? const <SessionItem>[])
        .where((s) => q.isEmpty || (s.title ?? '').toLowerCase().contains(q))
        .toList();
    return Dialog(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520, maxHeight: 560),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 18, 20, 12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  const Expanded(
                    child: Text(
                      'History',
                      style: TextStyle(
                        fontSize: 16,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  QuietIconButton(
                    icon: Icons.close,
                    tooltip: 'Close',
                    onPressed: () => Navigator.of(context).pop(),
                  ),
                ],
              ),
              const SizedBox(height: 10),
              TextField(
                controller: _search,
                autofocus: true,
                onChanged: (_) => setState(() {}),
                decoration: InputDecoration(
                  hintText: 'Search History',
                  prefixIcon: Icon(Icons.search, size: 18, color: c.muted),
                  isDense: true,
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(8),
                  ),
                ),
              ),
              const SizedBox(height: 8),
              Expanded(
                child: _error != null
                    ? Center(child: Text(_error!))
                    : _all == null
                    ? const Center(child: CircularProgressIndicator())
                    : shown.isEmpty
                    ? Center(
                        child: Text(
                          q.isEmpty ? 'Nothing here yet.' : 'No matches.',
                          style: Theme.of(context).textTheme.bodySmall,
                        ),
                      )
                    : ListView(
                        children: [
                          for (final s in shown)
                            ListTile(
                              dense: true,
                              shape: RoundedRectangleBorder(
                                borderRadius: BorderRadius.circular(8),
                              ),
                              leading: Icon(
                                s.source == 'telegram'
                                    ? Icons.send_outlined
                                    : Icons.chat_bubble_outline,
                                size: 16,
                                color: c.muted,
                              ),
                              title: Text(
                                s.title ?? 'Untitled',
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                              subtitle: Text(
                                '${kind(s, head)} · ${relativeTime(s.updatedAt)}',
                                style: TextStyle(
                                  fontSize: 11.5,
                                  color: c.muted,
                                ),
                              ),
                              trailing: Checkbox(
                                value: _picked.contains(s.id),
                                onChanged: s.messages == 0
                                    ? null
                                    : (v) => setState(
                                        () => v == true
                                            ? _picked.add(s.id)
                                            : _picked.remove(s.id),
                                      ),
                              ),
                              onTap: () {
                                Navigator.of(context).pop();
                                if (s.id != head) widget.store.openThread(s.id);
                              },
                            ),
                        ],
                      ),
              ),
              if (_picked.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Row(
                    children: [
                      Text(
                        '${_picked.length} picked',
                        style: Theme.of(context).textTheme.bodySmall,
                      ),
                      const Spacer(),
                      FilledButton.icon(
                        icon: const Icon(
                          Icons.drive_file_move_outline,
                          size: 16,
                        ),
                        label: const Text('Move to channel…'),
                        onPressed: () {
                          final picked = _picked.toList();
                          final store = widget.store;
                          final nav = Navigator.of(context);
                          final host = nav.context;
                          nav.pop();
                          showMoveDialog(host, store, picked);
                        },
                      ),
                    ],
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
