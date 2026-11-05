import 'package:flutter/material.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'channel_view.dart';
import 'items.dart';
import 'move_dialog.dart';
import 'widgets.dart';

/// The right pane when a conversation is open to read: a History entry, a
/// conversation moved into a channel, the run behind a post. There is no
/// composer: a conversation goes on in its stream or channel, which is
/// flat (DECISIONS.md "Flat conversations everywhere").
class ThreadPane extends StatelessWidget {
  const ThreadPane({super.key, required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final session = store.thread;
    final items = groupApprovals(store.threadTranscript?.items ?? const []);
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
            child: ListView.builder(
              reverse: true,
              padding: const EdgeInsets.symmetric(vertical: 14),
              itemCount: items.length,
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
