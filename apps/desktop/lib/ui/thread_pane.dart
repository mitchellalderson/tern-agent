import 'package:flutter/material.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'channel_view.dart';
import 'feed.dart';
import 'items.dart';
import 'move_dialog.dart';
import 'widgets.dart';

/// The right pane when a conversation is open to read: a History entry, a
/// conversation moved into a channel, the run behind a post. There is no
/// composer: a conversation goes on in its stream or channel, which is
/// flat (DECISIONS.md "Flat conversations everywhere").
class ThreadPane extends StatefulWidget {
  const ThreadPane({super.key, required this.store});

  final TernStore store;

  @override
  State<ThreadPane> createState() => _ThreadPaneState();
}

class _ThreadPaneState extends State<ThreadPane> {
  final _window = FeedWindow();
  String? _open;
  var _loadingEarlier = false;

  @override
  void initState() {
    super.initState();
    _window.addListener(_changed);
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _window
      ..removeListener(_changed)
      ..dispose();
    super.dispose();
  }

  /// Widen the window; at its end, fetch the page of messages before it.
  Future<void> _showEarlier(int hidden) async {
    if (hidden > 0) {
      _window.showEarlier();
      return;
    }
    setState(() => _loadingEarlier = true);
    await widget.store.loadOlderThread();
    if (!mounted) return;
    _loadingEarlier = false;
    _window.showEarlier();
  }

  @override
  Widget build(BuildContext context) {
    final store = widget.store;
    final c = context.colors;
    final session = store.thread;
    if (session != _open) {
      if (_open != null) _window.reset();
      _open = session;
    }
    final all = store.threadTranscript?.items ?? const <Item>[];
    final hiddenEarlier = _window.hiddenEarlier(all.length);
    final items = groupApprovals(_window.slice(all));
    final title =
        store.sessions.where((s) => s.id == session).firstOrNull?.title ??
        'Conversation';
    final waiting = store.waitingForThread();
    return Container(
      color: c.background,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          WindowDragArea(
            child: Container(
              height: titleBarInset + 12,
              padding: const EdgeInsets.fromLTRB(16, 0, 8, 0),
              child: Row(
                children: [
                  Icon(Icons.forum_outlined, size: 15, color: c.muted),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 14,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  if (session != null)
                    QuietIconButton(
                      icon: Icons.drive_file_move_outline,
                      tooltip: 'Move to a channel',
                      onPressed: () =>
                          showMoveDialog(context, store, [session]),
                    ),
                  QuietIconButton(
                    icon: Icons.close,
                    tooltip: 'Close',
                    onPressed: store.closeThread,
                  ),
                ],
              ),
            ),
          ),
          const Hairline(),
          if (waiting case (:final channel, :final count))
            WaitingIn(store: store, channelId: channel, count: count),
          Expanded(
            child: FeedList(
              window: _window,
              count: items.length,
              total: all.length,
              hiddenEarlier: hiddenEarlier,
              moreEarlier: store.threadHasEarlier,
              loadingEarlier: _loadingEarlier,
              onShowEarlier: () => _showEarlier(hiddenEarlier),
              padding: const EdgeInsets.symmetric(vertical: 14),
              keyFor: (i) => itemKey(items[items.length - 1 - i]),
              itemBuilder: (context, i) => Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 14,
                  vertical: 6,
                ),
                child: ItemView(
                  item: items[items.length - 1 - i],
                  store: store,
                  session: session,
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
